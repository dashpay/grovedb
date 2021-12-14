use grovedb::Element;
use neon::prelude::*;

pub fn element_to_js_value<'a, C: Context<'a>>(element: Element, cx: &mut C) -> NeonResult<Handle<'a, JsValue>> {
    let js_value = match element {
        Element::Item(item) => {
            let js_element = JsBuffer::external(cx, item.clone());
            js_element.upcast()
        }
        Element::Reference(reference) => {
            let js_array: Handle<JsArray> = cx.empty_array();

            for (index, bytes) in reference.iter().enumerate() {
                let js_buffer = JsBuffer::external(cx, bytes.clone());
                js_array.set(cx, index as u32, js_buffer.as_value(cx))?;
            }

            js_array.upcast()
        }
        Element::Tree(tree) => {
            let js_element = JsBuffer::external(cx, tree.clone());
            js_element.upcast()
        }
    };

    NeonResult::Ok(js_value)
}