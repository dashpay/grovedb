use crate::Path;

/// Result of finding the common prefix between two paths.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CommonPathResult {
    /// The shared prefix segments common to both paths.
    pub common_path: Path,
    /// The remaining segments of the left path after the common prefix.
    pub left_path_leftovers: Path,
    /// The remaining segments of the right path after the common prefix.
    pub right_path_leftovers: Path,
}

impl CommonPathResult {
    /// Splits two paths into their common prefix and the remaining segments.
    pub fn from_paths(left: &Path, right: &Path) -> Self {
        if left.eq(right) {
            return CommonPathResult {
                common_path: left.clone(),
                left_path_leftovers: vec![],
                right_path_leftovers: vec![],
            };
        }
        let mut common_path = vec![];
        let mut left_path_leftovers = vec![];
        let mut right_path_leftovers = vec![];
        for (ours_key, theirs_key) in left.iter().zip(right.iter()) {
            if ours_key != theirs_key {
                break;
            } else {
                common_path.push(ours_key.clone());
            }
        }
        let common_length = common_path.len();
        left_path_leftovers.extend_from_slice(left.split_at(common_length).1);
        right_path_leftovers.extend_from_slice(right.split_at(common_length).1);
        CommonPathResult {
            common_path,
            left_path_leftovers,
            right_path_leftovers,
        }
    }
}
