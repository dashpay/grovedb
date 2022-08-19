/// Reference path variants
pub enum ReferencePathType {
    AbsolutePath(Vec<Vec<u8>>)
}

pub struct ReferencePath {
    reference_path_type: ReferencePathType,
    path: Vec<Vec<u8>>,
}

impl ReferencePath {
    pub fn from_reference_path_type(reference_path_type: ReferencePathType, reference_path: Vec<Vec<u8>>) -> Self {
        return ReferencePath {
            reference_path_type,
            path: vec![b"a".to_vec()],
        }
    }
}