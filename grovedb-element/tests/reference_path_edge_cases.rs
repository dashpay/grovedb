use grovedb_element::{
    error::ElementError,
    reference_path::{
        path_from_reference_path_type, path_from_reference_qualified_path_type,
        util::path_as_slices_hex_to_ascii, ReferencePathType,
    },
};
use grovedb_path::{SubtreePath, SubtreePathBuilder};
use integer_encoding::VarInt;

fn assert_invalid_input(result: Result<Vec<Vec<u8>>, ElementError>) {
    assert!(matches!(
        result,
        Err(ElementError::InvalidInput(
            "reference stored path cannot satisfy reference constraints"
        ))
    ));
}

#[test]
fn reference_path_conversion_error_paths_are_covered() {
    let empty_qualified: [&[u8]; 0] = [];
    assert!(matches!(
        path_from_reference_qualified_path_type(
            ReferencePathType::SiblingReference(b"x".to_vec()),
            &empty_qualified,
        ),
        Err(ElementError::CorruptedPath(msg)) if msg.contains("qualified path should always have an element")
    ));

    assert_invalid_input(path_from_reference_path_type(
        ReferencePathType::UpstreamRootHeightReference(2, vec![b"x".to_vec()]),
        &[b"a".as_ref()],
        None,
    ));

    assert_invalid_input(path_from_reference_path_type(
        ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
            1,
            vec![b"x".to_vec()],
        ),
        &empty_qualified,
        None,
    ));

    assert_invalid_input(path_from_reference_path_type(
        ReferencePathType::UpstreamFromElementHeightReference(2, vec![b"x".to_vec()]),
        &[b"a".as_ref()],
        None,
    ));

    assert_invalid_input(path_from_reference_path_type(
        ReferencePathType::CousinReference(b"x".to_vec()),
        &empty_qualified,
        Some(b"k"),
    ));

    assert!(matches!(
        path_from_reference_path_type(
            ReferencePathType::CousinReference(b"x".to_vec()),
            &[b"a".as_ref()],
            None,
        ),
        Err(ElementError::InvalidInput(
            "cousin reference must supply a key"
        ))
    ));

    assert_invalid_input(path_from_reference_path_type(
        ReferencePathType::RemovedCousinReference(vec![b"x".to_vec()]),
        &empty_qualified,
        Some(b"k"),
    ));

    assert!(matches!(
        path_from_reference_path_type(
            ReferencePathType::RemovedCousinReference(vec![b"x".to_vec()]),
            &[b"a".as_ref()],
            None,
        ),
        Err(ElementError::InvalidInput(
            "cousin reference must supply a key"
        ))
    ));
}

#[test]
fn absolute_qualified_path_error_paths_are_covered() {
    let path: SubtreePathBuilder<&[u8]> = SubtreePathBuilder::owned_from_iter([b"a".as_ref()]);

    let err = ReferencePathType::UpstreamRootHeightReference(2, vec![])
        .absolute_qualified_path(path.clone(), b"k")
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));

    let empty_builder: SubtreePathBuilder<&[u8]> =
        SubtreePathBuilder::owned_from_iter(std::iter::empty::<&[u8]>());
    let err = ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(1, vec![])
        .absolute_qualified_path(empty_builder, b"k")
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));

    let err = ReferencePathType::UpstreamFromElementHeightReference(2, vec![])
        .absolute_qualified_path(path.clone(), b"k")
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));

    let err = ReferencePathType::CousinReference(b"x".to_vec())
        .absolute_qualified_path(SubtreePathBuilder::new(), b"k")
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));

    let err = ReferencePathType::RemovedCousinReference(vec![b"x".to_vec()])
        .absolute_qualified_path(SubtreePathBuilder::new(), b"k")
        .unwrap_err();
    assert!(matches!(
        err,
        ElementError::InvalidInput("reference stored path cannot satisfy reference constraints")
    ));
}

#[test]
fn invert_none_and_conversion_wrappers_are_covered() {
    let long_append_path = vec![vec![1u8]; (u8::MAX as usize) + 1];
    let segments = [b"a".as_ref(), b"b".as_ref()];
    let path: SubtreePath<_> = (&segments).into();

    assert!(
        ReferencePathType::UpstreamFromElementHeightReference(1, long_append_path.clone())
            .invert(path.clone(), b"k")
            .is_none()
    );
    assert!(ReferencePathType::RemovedCousinReference(long_append_path)
        .invert(path.clone(), b"k")
        .is_none());

    let empty_path: SubtreePath<'_, &[u8]> = (&[] as &[&[u8]]).into();
    assert!(ReferencePathType::CousinReference(b"x".to_vec())
        .invert(empty_path, b"k")
        .is_none());

    let current_qualified = [b"r".as_ref(), b"p".as_ref(), b"k".as_ref()];
    let path_from_method = ReferencePathType::SiblingReference(b"sib".to_vec())
        .clone()
        .absolute_path_using_current_qualified_path(&current_qualified)
        .unwrap();
    let path_direct = path_from_reference_qualified_path_type(
        ReferencePathType::SiblingReference(b"sib".to_vec()),
        &current_qualified,
    )
    .unwrap();
    assert_eq!(path_from_method, path_direct);

    let from_absolute_method = ReferencePathType::AbsolutePathReference(vec![b"a".to_vec()])
        .absolute_path(&[b"unused".as_ref()], None)
        .unwrap();
    assert_eq!(from_absolute_method, vec![b"a".to_vec()]);
}

#[test]
fn serialized_size_display_and_util_helpers_are_covered() {
    let absolute = ReferencePathType::AbsolutePathReference(vec![b"ab".to_vec(), b"c".to_vec()]);
    let removed = ReferencePathType::RemovedCousinReference(vec![b"ab".to_vec(), b"c".to_vec()]);
    let upstream = ReferencePathType::UpstreamRootHeightReference(2, vec![b"x".to_vec()]);
    let upstream_parent = ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
        2,
        vec![b"x".to_vec()],
    );
    let upstream_from =
        ReferencePathType::UpstreamFromElementHeightReference(1, vec![b"xy".to_vec()]);
    let cousin = ReferencePathType::CousinReference(b"xyz".to_vec());
    let sibling = ReferencePathType::SiblingReference(b"w".to_vec());

    let absolute_expected = 1 + (2 + 2usize.required_space()) + (1 + 1usize.required_space());
    assert_eq!(absolute.serialized_size(), absolute_expected);
    assert_eq!(removed.serialized_size(), absolute_expected);

    let upstream_expected = 1 + 1 + (1 + 1usize.required_space());
    assert_eq!(upstream.serialized_size(), upstream_expected);
    assert_eq!(upstream_parent.serialized_size(), upstream_expected);

    let upstream_from_expected = 1 + 1 + (2 + 2usize.required_space());
    assert_eq!(upstream_from.serialized_size(), upstream_from_expected);

    let cousin_expected = 1 + 3 + 3usize.required_space();
    assert_eq!(cousin.serialized_size(), cousin_expected);

    let sibling_expected = 1 + 1 + 1usize.required_space();
    assert_eq!(sibling.serialized_size(), sibling_expected);

    assert_eq!(
        path_as_slices_hex_to_ascii(&[b"abc".as_ref(), &[0, 255]]),
        "abc/0x00ff"
    );

    assert_eq!(
        format!(
            "{}",
            ReferencePathType::AbsolutePathReference(vec![b"abc".to_vec()])
        ),
        "AbsolutePathReference(616263(abc))"
    );
    assert_eq!(
        format!(
            "{}",
            ReferencePathType::UpstreamRootHeightReference(1, vec![b"ab".to_vec()])
        ),
        "UpstreamRootHeightReference(1, 6162(ab))"
    );
    assert_eq!(
        format!(
            "{}",
            ReferencePathType::UpstreamRootHeightWithParentPathAdditionReference(
                1,
                vec![b"ab".to_vec()]
            )
        ),
        "UpstreamRootHeightWithParentPathAdditionReference(1, 6162(ab))"
    );
    assert_eq!(
        format!(
            "{}",
            ReferencePathType::UpstreamFromElementHeightReference(1, vec![b"ab".to_vec()])
        ),
        "UpstreamFromElementHeightReference(1, 6162(ab))"
    );
    assert_eq!(
        format!("{}", ReferencePathType::CousinReference(b"z".to_vec())),
        "CousinReference(7a)"
    );
    assert_eq!(
        format!(
            "{}",
            ReferencePathType::RemovedCousinReference(vec![b"ab".to_vec(), b"cd".to_vec()])
        ),
        "RemovedCousinReference(6162(ab)/6364(cd))"
    );
    assert_eq!(
        format!("{}", ReferencePathType::SiblingReference(b"s".to_vec())),
        "SiblingReference(73)"
    );
}
