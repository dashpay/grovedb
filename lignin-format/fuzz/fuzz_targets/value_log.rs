//! Value-log files, records, values and the value pointer
//! (`value-log-format.md` sections 1 to 4).

#![no_main]

use libfuzzer_sys::fuzz_target;
use lignin_format::{
    value_log::{
        self, read_value, record_len, write_value, ValuePlace, ValuePointer, ValueRecord,
        RECORD_HEADER_LEN,
    },
    FormatVersion, PageId,
};
use lignin_format_fuzz::{default_frame_len, ok, sealed_file, sealed_records, Input};

const V1: FormatVersion = FormatVersion::V1;

fuzz_target!(|data: &[u8]| {
    let Some(input) = Input::split(data) else {
        return;
    };
    match input.control >> 1 & 3 {
        0 | 1 => file(&sealed_file(&input, default_frame_len)),
        2 => records(&sealed_records(&input, default_frame_len)),
        _ => places(&input),
    }
});

fn file(file: &[u8]) {
    let Some(values) = ok(value_log::scan(file)) else {
        return;
    };
    let id = values.file();
    assert_eq!(values.header().file_id(), Some(id));
    let mut values = values;
    let mut offset = values.offset();
    while let Some(next) = values.next() {
        let Some((pointer, value)) = ok(next) else {
            assert_eq!(values.offset(), offset);
            assert!(values.next().is_none());
            break;
        };
        assert_eq!(pointer.file(), id);
        assert_eq!(pointer.offset(), offset);
        assert_eq!(pointer.value_len(), value.value_len());
        assert_eq!(ValuePointer::decode(pointer.encode()), Ok(pointer));
        assert_eq!(pointer.extent_len(V1), value.encoded_len() as u64);
        assert_eq!(
            value_log::extent_len(value.value_len(), V1),
            value.encoded_len() as u64
        );
        assert_eq!(pointer.end(V1), offset + value.encoded_len() as u64);

        let extent = &file[offset as usize..];
        assert_eq!(ok(pointer.read(extent, value.owner(), V1)), Some(value));
        let other = PageId::new(value.owner().get().wrapping_add(1)).unwrap_or(PageId::MIN);
        assert!(ok(pointer.read(extent, other, V1)).is_none());

        let bytes = value.to_vec();
        assert_eq!(bytes.len(), value.value_len() as usize);
        assert_eq!(value.contiguous(), Some(bytes.as_slice()));
        assert_eq!(value.parts().count(), 1);
        let mut written = Vec::new();
        write_value(value.owner(), &bytes, &mut written).expect("a value that was read");
        assert_eq!(written, extent[..value.encoded_len()]);
        offset = values.offset();
    }
}

fn records(bytes: &[u8]) {
    let mut rest = bytes;
    while !rest.is_empty() {
        let len = ok(record_len(rest, V1));
        if let Some((value, after)) = ok(read_value(rest, V1)) {
            assert_eq!(value.encoded_len(), rest.len() - after.len());
            let _ = value.to_vec();
        }
        let Some((record, after)) = ok(ValueRecord::decode(rest, V1)) else {
            return;
        };
        assert_eq!(len, Some(record.encoded_len()));
        assert_eq!(record.encoded_len(), rest.len() - after.len());
        assert_eq!(
            record.encoded_len(),
            RECORD_HEADER_LEN + record.data().len()
        );
        assert!(record.data().len() <= record.value_len() as usize);
        let _ = (record.owner(), record.part());
        rest = after;
    }
}

fn places(input: &Input<'_>) {
    let bytes = input.bytes;
    let mut rest = bytes;
    while let Some((place, after)) = rest.split_first_chunk::<{ ValuePlace::ENCODED_LEN }>() {
        rest = after;
        let pointer = ok(ValuePointer::decode(*place));
        if let Some(pointer) = pointer {
            assert_eq!(pointer.encode(), *place);
            assert_eq!(
                ok(ValuePointer::new(
                    pointer.file(),
                    pointer.offset(),
                    pointer.value_len()
                )),
                Some(pointer)
            );
            assert!(pointer.value_len() > 0);
            assert!(pointer.offset() <= value_log::MAX_VALUE_OFFSET);
            let _ = pointer.end(V1);
            let extent = sealed_records(
                &Input {
                    control: input.control,
                    bytes: rest,
                },
                default_frame_len,
            );
            let owner = PageId::new(input.control.into()).unwrap_or(PageId::MIN);
            if let Some(value) = ok(pointer.read(&extent, owner, V1)) {
                assert_eq!(value.owner(), owner);
                assert_eq!(value.value_len(), pointer.value_len());
            }
        }
        let Some(decoded) = ok(ValuePlace::decode(*place)) else {
            assert!(pointer.is_none());
            continue;
        };
        assert_eq!(decoded.encode(), *place);
        assert_eq!(decoded.pointer(), pointer);
        assert_eq!(decoded.is_pending(), decoded.pending().is_some());
        assert_eq!(decoded.is_pending(), pointer.is_none());
        if let Some(pending) = decoded.pending() {
            assert_eq!(pending.value_len(), decoded.value_len());
            let again =
                lignin_format::value_log::PendingValue::new(pending.id(), pending.value_len());
            assert_eq!(ok(again), Some(pending));
        }
    }
    for window in bytes.windows(4).take(32) {
        let len = u32::from_le_bytes(window.try_into().unwrap()) as usize;
        if let Some(value_len) = ok(value_log::checked_value_len(len)) {
            assert_eq!(value_len as usize, len);
            assert!(value_log::extent_len(value_len, V1) >= u64::from(value_len));
        }
    }
}
