/// Reference path variants
pub enum ReferencePath {
    AbsolutePath(Vec<Vec<u8>>)
}

impl ReferencePath {
    fn get_reference_path(&self, current_path: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
       match self {
           ReferencePath::AbsolutePath(path) => path
       }
    }
}