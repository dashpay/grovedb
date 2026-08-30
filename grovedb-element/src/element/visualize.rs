//! Visualize

use std::{
    fmt,
    io::{Result, Write},
};

use grovedb_visualize::{Drawer, Visualize};

use crate::{element::Element, visualize_helpers::visualize_to_vec};

impl Visualize for Element {
    fn visualize<W: Write>(&self, mut drawer: Drawer<W>) -> Result<Drawer<W>> {
        match self {
            Element::Item(value, flags) => {
                drawer.write(b"item: ")?;
                drawer = value.visualize(drawer)?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::SumItem(value, flags) => {
                drawer.write(format!("sum_item: {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::BidirectionalReference(reference, flags) => {
                drawer.write(
                    format!(
                        "bidi_ref: [forward: {}, cascade: {}, max_hop: {}, backrefs: {}]",
                        reference.forward_reference_path,
                        reference.cascade_on_update,
                        reference
                            .max_hop
                            .map_or("None".to_string(), |h| h.to_string()),
                        reference.backward_references.len(),
                    )
                    .as_bytes(),
                )?;
                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ItemWithBackwardsReferences(value, _, flags) => {
                drawer.write(b"item_with_backwards_references: ")?;
                drawer = value.visualize(drawer)?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::SumItemWithBackwardsReferences(value, _, flags) => {
                drawer.write(format!("sum_item_with_backwards_references: {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ItemWithSumItemWithBackwardsReferences(value, sum_value, _, flags) => {
                drawer.write(b"item_with_sum_item_with_backwards_references: ")?;
                drawer = value.visualize(drawer)?;
                drawer.write(format!(", sum: {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::Reference(_ref, ..) => {
                drawer.write(b"ref")?;
                // drawer.write(b"ref: [path: ")?;
                // let mut path_iter = path.iter();
                // if let Some(first) = path_iter.next() {
                //     drawer = first.visualize(drawer)?;
                // }
                // for p in path_iter {
                //     drawer.write(b", ")?;
                //     drawer = p.visualize(drawer)?;
                // }
                // drawer.write(b"]")?;
            }
            Element::Tree(root_key, flags) => {
                drawer.write(b"tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::SumTree(root_key, value, flags) => {
                drawer.write(b"sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::BigSumTree(root_key, value, flags) => {
                drawer.write(b"big_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::CountTree(root_key, value, flags) => {
                drawer.write(b"count_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::CountSumTree(root_key, count_value, sum_value, flags) => {
                drawer.write(b"count_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!("count: {count_value}, sum {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }

            Element::ProvableCountTree(root_key, value, flags) => {
                drawer.write(b"provable_count_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ProvableCountSumTree(root_key, count_value, sum_value, flags) => {
                drawer.write(b"provable_count_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!("count: {count_value}, sum {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ItemWithSumItem(value, sum_value, flags) => {
                drawer.write(b"item_with_sum_item: ")?;
                drawer = value.visualize(drawer)?;
                drawer.write(format!(" {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::CommitmentTree(count, chunk_power, flags) => {
                drawer.write(
                    format!("commitment_tree: count: {count} chunk_power: {chunk_power}",)
                        .as_bytes(),
                )?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::MmrTree(mmr_size, flags) => {
                drawer.write(format!("mmr_tree: mmr_size: {mmr_size}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::BulkAppendTree(total_count, chunk_power, flags) => {
                drawer.write(
                    format!(
                        "bulk_append_tree: total_count: {total_count} chunk_power: {chunk_power}",
                    )
                    .as_bytes(),
                )?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::DenseAppendOnlyFixedSizeTree(count, height, flags) => {
                drawer.write(format!("dense_tree: count: {count} height: {height}",).as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::PrivateDocumentStore(total_count, entry_size, chunk_power, flags) => {
                drawer.write(
                    format!(
                        "private_document_store: count: {total_count} entry_size: {entry_size} \
                         chunk_power: {chunk_power}",
                    )
                    .as_bytes(),
                )?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::NonCounted(inner) => {
                drawer.write(b"non_counted(")?;
                drawer = inner.visualize(drawer)?;
                drawer.write(b")")?;
            }
            Element::ProvableSumIndexedTree(primary, secondary, sum_value, flags) => {
                drawer.write(b"provable_sum_indexed_tree: primary=")?;
                drawer = primary.as_deref().visualize(drawer)?;
                drawer.write(b", secondary=")?;
                drawer = secondary.as_deref().visualize(drawer)?;
                drawer.write(format!(", sum: {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ProvableCountIndexedTree(primary, secondary, count_value, flags) => {
                drawer.write(b"provable_count_indexed_tree: primary=")?;
                drawer = primary.as_deref().visualize(drawer)?;
                drawer.write(b", secondary=")?;
                drawer = secondary.as_deref().visualize(drawer)?;
                drawer.write(format!(", count: {count_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ProvableCountProvableSumIndexedTree(
                primary,
                count_value,
                sum_value,
                axes,
                flags,
            ) => {
                drawer.write(b"provable_count_provable_sum_indexed_tree: primary=")?;
                drawer = primary.as_deref().visualize(drawer)?;
                drawer.write(
                    format!(", count: {count_value}, sum: {sum_value}, axes: [").as_bytes(),
                )?;
                let mut first = true;
                for (tag, sk) in axes {
                    if !first {
                        drawer.write(b", ")?;
                    }
                    first = false;
                    drawer.write(format!("({tag}, ").as_bytes())?;
                    drawer = sk.as_deref().visualize(drawer)?;
                    drawer.write(b")")?;
                }
                drawer.write(b"]")?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::NotSummed(inner) => {
                drawer.write(b"not_summed(")?;
                drawer = inner.visualize(drawer)?;
                drawer.write(b")")?;
            }
            Element::ProvableSumTree(root_key, value, flags) => {
                drawer.write(b"provable_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!(" {value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::ProvableCountProvableSumTree(root_key, count_value, sum_value, flags) => {
                drawer.write(b"provable_count_provable_sum_tree: ")?;
                drawer = root_key.as_deref().visualize(drawer)?;
                drawer.write(format!("count: {count_value}, sum {sum_value}").as_bytes())?;

                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
            Element::NotCountedOrSummed(inner) => {
                drawer.write(b"not_counted_or_summed(")?;
                drawer = inner.visualize(drawer)?;
                drawer.write(b")")?;
            }
            Element::ReferenceWithSumItem(_ref, _max_hop, sum_value, flags) => {
                drawer.write(format!("ref_with_sum_item: {sum_value}").as_bytes())?;
                if let Some(f) = flags
                    && !f.is_empty()
                {
                    drawer = f.visualize(drawer)?;
                }
            }
        }
        Ok(drawer)
    }
}

impl fmt::Debug for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut v = Vec::new();
        visualize_to_vec(&mut v, self);

        f.write_str(&String::from_utf8_lossy(&v))
    }
}

#[cfg(test)]
mod tests {
    use grovedb_visualize::to_hex;

    #[test]
    fn visualize_backward_references_family() {
        let render = |e: &Element| {
            let mut out = Vec::new();
            let drawer = Drawer::new(&mut out);
            e.visualize(drawer).expect("visualize IO error");
            String::from_utf8_lossy(&out).into_owned()
        };

        let bidi = Element::BidirectionalReference(crate::BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(b"t".to_vec()),
            cascade_on_update: true,
            max_hop: Some(3),
            backward_references: Vec::new(),
            flags: Some(vec![1]),
        });
        let s = render(&bidi);
        assert!(
            s.starts_with("bidi_ref: [") && s.contains("cascade: true") && s.contains("max_hop: 3"),
            "got: {s}"
        );

        let bidi_plain = Element::BidirectionalReference(crate::BidirectionalReference {
            forward_reference_path: ReferencePathType::SiblingReference(b"t".to_vec()),
            cascade_on_update: false,
            max_hop: None,
            backward_references: Vec::new(),
            flags: None,
        });
        assert!(render(&bidi_plain).contains("max_hop: None"));

        let item = Element::ItemWithBackwardsReferences(b"v".to_vec(), Vec::new(), Some(vec![2]));
        let s = render(&item);
        assert!(
            s.starts_with("item_with_backwards_references: "),
            "got: {s}"
        );
        let item_plain = Element::ItemWithBackwardsReferences(b"v".to_vec(), Vec::new(), None);
        render(&item_plain);

        let sum = Element::SumItemWithBackwardsReferences(-7, Vec::new(), Some(vec![3]));
        let s = render(&sum);
        assert!(
            s.starts_with("sum_item_with_backwards_references: -7"),
            "got: {s}"
        );
        let sum_plain = Element::SumItemWithBackwardsReferences(-7, Vec::new(), None);
        render(&sum_plain);
    }

    use super::*;
    use crate::reference_path::ReferencePathType;

    #[test]
    fn test_element_item_str() {
        let v = b"ayylmao".to_vec();
        let e = Element::new_item(v.clone());
        let element_hex = to_hex(&v);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!(
                "item: [hex: {element_hex}, str: {}]",
                String::from_utf8_lossy(&v)
            ),
            String::from_utf8_lossy(result.as_ref())
        );
    }

    #[test]
    fn test_element_item_no_tr() {
        let v = vec![1, 3, 3, 7, 255];
        let e = Element::new_item(v.clone());
        let element_hex = to_hex(&v);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!("item: [hex: {element_hex}]"),
            String::from_utf8_lossy(result.as_ref())
        );
    }

    #[test]
    #[ignore]
    fn test_visualize_reference() {
        let p1 = b"ayy".to_vec();
        let p2 = b"lmao".to_vec();
        let e = Element::new_reference(ReferencePathType::AbsolutePathReference(vec![
            p1.clone(),
            p2.clone(),
        ]));
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        assert_eq!(
            format!(
                "ref: [path: [hex: {p1_hex}, str: {p1}], [hex: {p2_hex}, str: {p2}]]",
                p1 = String::from_utf8_lossy(&p1),
                p2 = String::from_utf8_lossy(&p2),
                p1_hex = to_hex(&p1),
                p2_hex = to_hex(&p2),
            ),
            String::from_utf8_lossy(result.as_ref())
        );
    }

    #[test]
    fn test_visualize_provable_sum_indexed_tree_empty() {
        let e = Element::ProvableSumIndexedTree(None, None, 0, None);
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        let rendered = String::from_utf8_lossy(result.as_ref()).into_owned();
        assert!(
            rendered.contains("provable_sum_indexed_tree"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("sum: 0"), "rendered: {rendered}");
    }

    #[test]
    fn test_visualize_provable_count_provable_sum_indexed_tree() {
        let e = Element::ProvableCountProvableSumIndexedTree(
            Some(vec![0x11]),
            5,
            42,
            vec![(0, Some(vec![0xab])), (1, None)],
            None,
        );
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        let rendered = String::from_utf8_lossy(result.as_ref()).into_owned();
        assert!(
            rendered.contains("provable_count_provable_sum_indexed_tree"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("count: 5"), "rendered: {rendered}");
        assert!(rendered.contains("sum: 42"), "rendered: {rendered}");
    }

    #[test]
    fn test_visualize_provable_count_indexed_tree() {
        let e = Element::ProvableCountIndexedTree(
            Some(vec![0x11]),
            Some(vec![0x22]),
            42,
            Some(vec![1]),
        );
        let mut result = Vec::new();
        let drawer = Drawer::new(&mut result);
        e.visualize(drawer).expect("visualize IO error");
        let rendered = String::from_utf8_lossy(result.as_ref()).into_owned();
        assert!(
            rendered.contains("provable_count_indexed_tree"),
            "rendered: {rendered}"
        );
        assert!(rendered.contains("count: 42"), "rendered: {rendered}");
    }
}
