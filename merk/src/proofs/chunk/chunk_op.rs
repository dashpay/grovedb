// MIT LICENSE
//
// Copyright (c) 2021 Dash Core Group
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.

use std::io::{Read, Write};

use ed::{Decode, Encode};
use integer_encoding::{VarInt, VarIntReader};

use crate::proofs::{
    chunk::{
        chunk::{LEFT, RIGHT},
        util::max_traversal_instruction_len,
    },
    Op,
};

/// Longest chunk id the decoder accepts.
///
/// Merk stores node heights as `u8`, so no tree is taller than `u8::MAX` and
/// no traversal instruction longer than this can address a node. The declared
/// length is checked against it before the instruction buffer is allocated
/// (issue #904).
const MAX_CHUNK_ID_LEN: usize = max_traversal_instruction_len(u8::MAX as usize);

/// Most ops the decoder reserves room for up front.
///
/// The declared op count is attacker-controlled and the reader's remaining
/// length is unknown, so the count only sizes the initial reservation up to
/// this many ops. Past that the vec grows as ops are actually decoded, which
/// keeps the allocation proportional to the input consumed (issue #904).
const MAX_PREALLOCATED_OPS: usize = 1024;

fn invalid_data_error(message: &'static str) -> ed::Error {
    ed::Error::IOError(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    ))
}

/// Represents the chunk generated from a given starting chunk id
#[derive(PartialEq, Debug)]
pub enum ChunkOp {
    /// A chunk identifier represented as a traversal instruction (sequence of
    /// left/right booleans).
    ChunkId(Vec<bool>),
    /// A chunk of proof operations.
    Chunk(Vec<Op>),
}

impl Encode for ChunkOp {
    fn encode_into<W: Write>(&self, dest: &mut W) -> ed::Result<()> {
        match self {
            Self::ChunkId(instruction) => {
                // write the marker then the len
                dest.write_all(&[0_u8])?;
                dest.write_all(instruction.len().encode_var_vec().as_slice())?;
                let instruction_as_binary: Vec<u8> = instruction
                    .iter()
                    .map(|v| if *v { 1_u8 } else { 0_u8 })
                    .collect();
                dest.write_all(&instruction_as_binary)?;
            }
            Self::Chunk(chunk) => {
                dest.write_all(&[1_u8])?;
                // chunk len represents the number of ops not the total encoding len of ops
                dest.write_all(chunk.len().encode_var_vec().as_slice())?;
                for op in chunk {
                    dest.write_all(&op.encode()?)?;
                }
            }
        }

        Ok(())
    }

    fn encoding_length(&self) -> ed::Result<usize> {
        Ok(match self {
            Self::ChunkId(instruction) => {
                1 + instruction.len().encode_var_vec().len() + instruction.len()
            }
            Self::Chunk(chunk) => {
                1 + chunk.len().encode_var_vec().len() + chunk.encoding_length()?
            }
        })
    }
}

impl Decode for ChunkOp {
    fn decode<R: Read>(input: R) -> ed::Result<Self> {
        let mut chunk_op = ChunkOp::ChunkId(vec![]);
        Self::decode_into(&mut chunk_op, input)?;
        Ok(chunk_op)
    }

    fn decode_into<R: Read>(&mut self, mut input: R) -> ed::Result<()> {
        let mut marker = [0_u8; 1];
        input.read_exact(&mut marker)?;

        match marker[0] {
            0 => {
                let length: usize = input.read_varint()?;
                if length > MAX_CHUNK_ID_LEN {
                    return Err(invalid_data_error(
                        "chunk id is longer than any tree is deep",
                    ));
                }
                let mut instruction_as_binary = vec![0_u8; length];
                input.read_exact(&mut instruction_as_binary)?;

                // same mapping as `vec_bytes_as_traversal_instruction`: anything
                // other than 0 or 1 is not a traversal step
                let instruction = instruction_as_binary
                    .into_iter()
                    .map(|v| match v {
                        1_u8 => Ok(LEFT),
                        0_u8 => Ok(RIGHT),
                        _ => Err(ed::Error::UnexpectedByte(v)),
                    })
                    .collect::<ed::Result<Vec<bool>>>()?;

                *self = ChunkOp::ChunkId(instruction);
            }
            1 => {
                let ops_length: usize = input.read_varint()?;
                let mut chunk = Vec::with_capacity(ops_length.min(MAX_PREALLOCATED_OPS));

                for _ in 0..ops_length {
                    let op = Decode::decode(&mut input)?;
                    chunk.push(op);
                }

                *self = ChunkOp::Chunk(chunk);
            }
            _ => return Err(ed::Error::UnexpectedByte(marker[0])),
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use ed::{Decode, Encode};
    use integer_encoding::VarInt;

    use crate::proofs::{
        chunk::{
            chunk::{LEFT, RIGHT},
            chunk_op::{ChunkOp, MAX_CHUNK_ID_LEN, MAX_PREALLOCATED_OPS},
        },
        Node, Op,
    };

    #[test]
    fn test_chunk_op_encoding() {
        let chunk_op = ChunkOp::ChunkId(vec![LEFT, RIGHT]);
        let encoded_chunk_op = chunk_op.encode().unwrap();
        assert_eq!(encoded_chunk_op, vec![0, 2, 1, 0]);
        assert_eq!(encoded_chunk_op.len(), chunk_op.encoding_length().unwrap());

        let chunk_op = ChunkOp::Chunk(vec![Op::Push(Node::Hash([0; 32])), Op::Child]);
        let encoded_chunk_op = chunk_op.encode().unwrap();
        let mut expected_encoding = vec![1, 2];
        expected_encoding.extend(Op::Push(Node::Hash([0; 32])).encode().unwrap());
        expected_encoding.extend(Op::Child.encode().unwrap());
        assert_eq!(encoded_chunk_op, expected_encoding);
        assert_eq!(encoded_chunk_op.len(), chunk_op.encoding_length().unwrap());
    }

    #[test]
    fn test_chunk_op_decoding() {
        let encoded_chunk_op = vec![0, 3, 1, 0, 1];
        let decoded_chunk_op = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap();
        assert_eq!(decoded_chunk_op, ChunkOp::ChunkId(vec![LEFT, RIGHT, LEFT]));

        let mut encoded_chunk_op = vec![1, 2];
        encoded_chunk_op.extend(Op::Push(Node::Hash([1; 32])).encode().unwrap());
        encoded_chunk_op.extend(Op::Push(Node::KV(vec![1], vec![2])).encode().unwrap());
        let decoded_chunk_op = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap();
        assert_eq!(
            decoded_chunk_op,
            ChunkOp::Chunk(vec![
                Op::Push(Node::Hash([1; 32])),
                Op::Push(Node::KV(vec![1], vec![2]))
            ])
        );
    }

    #[test]
    fn test_chunk_op_decoding_unexpected_marker() {
        let err = ChunkOp::decode([9u8].as_slice()).unwrap_err();
        assert!(matches!(err, ed::Error::UnexpectedByte(9)));
    }

    #[test]
    fn test_chunk_op_decoding_rejects_non_binary_chunk_id_values() {
        // issue #704: only 0 and 1 are traversal steps; every other byte used
        // to decode as `false`, giving one chunk id many encodings
        for bad_byte in [2u8, 3, 128, 255] {
            let encoded_chunk_op = vec![0, 4, 1, bad_byte, 0, 1];
            let err = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap_err();
            assert!(
                matches!(err, ed::Error::UnexpectedByte(byte) if byte == bad_byte),
                "byte {bad_byte} should be rejected, got {err:?}"
            );
        }
    }

    #[test]
    fn test_chunk_op_decoding_bounds_chunk_id_length_before_allocating() {
        // issue #904: a declared length no tree can have is refused outright,
        // without reserving a buffer for it
        let mut encoded_chunk_op = vec![0];
        encoded_chunk_op.extend(usize::MAX.encode_var_vec());
        let err = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap_err();
        assert!(
            matches!(&err, ed::Error::IOError(e) if e.kind() == std::io::ErrorKind::InvalidData),
            "got {err:?}"
        );

        let mut encoded_chunk_op = vec![0];
        encoded_chunk_op.extend((MAX_CHUNK_ID_LEN + 1).encode_var_vec());
        encoded_chunk_op.extend(vec![0u8; MAX_CHUNK_ID_LEN + 1]);
        let err = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap_err();
        assert!(
            matches!(&err, ed::Error::IOError(e) if e.kind() == std::io::ErrorKind::InvalidData),
            "got {err:?}"
        );
    }

    #[test]
    fn test_chunk_op_longest_chunk_id_round_trips() {
        let instruction: Vec<bool> = (0..MAX_CHUNK_ID_LEN).map(|i| i % 2 == 0).collect();
        let chunk_op = ChunkOp::ChunkId(instruction);
        let encoded_chunk_op = chunk_op.encode().unwrap();
        assert_eq!(encoded_chunk_op.len(), chunk_op.encoding_length().unwrap());
        assert_eq!(
            ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap(),
            chunk_op
        );
    }

    #[test]
    fn test_chunk_op_decoding_does_not_reserve_declared_op_count() {
        // issue #904: the op count used to go straight into
        // `Vec::with_capacity`, so this panicked on capacity overflow
        // instead of returning an error
        let mut encoded_chunk_op = vec![1];
        encoded_chunk_op.extend(usize::MAX.encode_var_vec());
        encoded_chunk_op.extend(Op::Child.encode().unwrap());
        let err = ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap_err();
        assert!(matches!(err, ed::Error::IOError(_)), "got {err:?}");
    }

    #[test]
    fn test_chunk_op_more_ops_than_preallocated_round_trip() {
        let chunk_op = ChunkOp::Chunk(vec![Op::Child; MAX_PREALLOCATED_OPS + 1]);
        let encoded_chunk_op = chunk_op.encode().unwrap();
        assert_eq!(
            ChunkOp::decode(encoded_chunk_op.as_slice()).unwrap(),
            chunk_op
        );
    }
}
