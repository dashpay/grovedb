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

//! Merk tree debug

use std::fmt::{Debug, Formatter, Result};

use colored::Colorize;

use super::{Link, TreeNode};

#[cfg(feature = "full")]
impl Debug for TreeNode {
    // TODO: unwraps should be results that bubble up
    fn fmt(&self, f: &mut Formatter) -> Result {
        fn traverse(
            f: &mut Formatter,
            cursor: &TreeNode,
            stack: &mut Vec<(Vec<u8>, Vec<u8>)>,
            left: bool,
        ) {
            if let Some(child_link) = cursor.link(true) {
                stack.push((child_link.key().to_vec(), cursor.key().to_vec()));
                if let Some(child_tree) = child_link.tree() {
                    traverse(f, child_tree, stack, true);
                } else {
                    traverse_pruned(f, child_link, stack, true);
                }
                stack.pop();
            }

            let depth = stack.len();

            if depth > 0 {
                // draw ancestor's vertical lines
                for (low, high) in stack.iter().take(depth - 1) {
                    let draw_line = cursor.key() > low && cursor.key() < high;
                    write!(f, "{}", if draw_line { " │  " } else { "    " }.dimmed()).unwrap();
                }
            }

            let prefix = if depth == 0 {
                ""
            } else if left {
                " ┌-"
            } else {
                " └-"
            };
            writeln!(
                f,
                "{}{}",
                prefix.dimmed(),
                format!("{:?}", cursor.key()).on_bright_black()
            )
            .unwrap();

            if let Some(child_link) = cursor.link(false) {
                stack.push((cursor.key().to_vec(), child_link.key().to_vec()));
                if let Some(child_tree) = child_link.tree() {
                    traverse(f, child_tree, stack, false);
                } else {
                    traverse_pruned(f, child_link, stack, false);
                }
                stack.pop();
            }
        }

        fn traverse_pruned(
            f: &mut Formatter,
            link: &Link,
            stack: &mut [(Vec<u8>, Vec<u8>)],
            left: bool,
        ) {
            let depth = stack.len();

            if depth > 0 {
                // draw ancestor's vertical lines
                for (low, high) in stack.iter().take(depth - 1) {
                    let draw_line = link.key() > low && link.key() < high;
                    write!(f, "{}", if draw_line { " │  " } else { "    " }.dimmed()).unwrap();
                }
            }

            let prefix = if depth == 0 {
                ""
            } else if left {
                " ┌-"
            } else {
                " └-"
            };
            writeln!(
                f,
                "{}{}",
                prefix.dimmed(),
                format!("{:?}", link.key()).blue()
            )
            .unwrap();
        }

        let mut stack = vec![];
        traverse(f, self, &mut stack, false);
        writeln!(f)
    }
}
