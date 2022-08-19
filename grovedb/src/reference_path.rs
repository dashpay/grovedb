/// Reference path variants
pub enum ReferencePathType {
    AbsolutePath(Vec<Vec<u8>>)
}

pub struct ReferencePath {
    pub path: Vec<Vec<u8>>,
}

impl ReferencePath {
    pub fn from_reference_path_type(reference_path_type: ReferencePathType) -> Self {
        return match reference_path_type {
            ReferencePathType::AbsolutePath(path) => {
                Self {
                    path
                }
            }
        }
    }
}