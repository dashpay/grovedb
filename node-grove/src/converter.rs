use grovedb::{Element};
use neon::{prelude::*, borrow::Borrow};

fn element_to_string(element: Element) -> String {
    match element {
        Element::Item(_) => { "item".to_string() }
        Element::Reference(_) => { "reference".to_string() }
        Element::Tree(_) => { "tree".to_string() }
    }
}

pub fn js_object_to_element<'a, C: Context<'a>>(js_object: Handle<JsObject>, cx: &mut C) -> NeonResult<Element> {
    let js_element_string = js_object.get(cx, "type")?.to_string(cx)?;
    let value = js_object.get(cx, "value")?;

    let element_string: String = js_element_string.value(cx);

    match element_string.as_str() {
        "item" => {
            let js_buffer = value.downcast_or_throw::<JsBuffer, _>(cx)?;
            let item = js_buffer_to_vec_u8(js_buffer, cx);
            Ok(Element::Item(item))
        },
        "reference" => {
            let js_array = value.downcast_or_throw::<JsArray, _>(cx)?;
            let reference = js_array_of_buffers_to_vec(js_array, cx)?;
            Ok(Element::Reference(reference))
        },
        "tree" => {
            let js_buffer = value.downcast_or_throw::<JsBuffer, _>(cx)?;
            let tree_vec = js_buffer_to_vec_u8(js_buffer, cx);
            Ok(Element::Tree(
                tree_vec
                    .try_into()
                    .or_else(|v: Vec<u8>| {
                        cx.throw_error(
                            format!("Tree buffer is expected to be 32 bytes long, but got {}", v.len())
                        )
                    })?
            ))
        }
        _ => {
            cx.throw_error(format!("Unexpected element type {}", element_string))
        }
    }
}

pub fn element_to_js_object<'a, C: Context<'a>>(element: Element, cx: &mut C) -> NeonResult<Handle<'a, JsValue>> {
    let js_object = cx.empty_object();
    let js_type_string = cx.string(element_to_string(element.clone()));
    js_object.set(cx, "type", js_type_string)?;

    let js_value: Handle<JsValue> = match element {
        Element::Item(item) => {
            let js_buffer = JsBuffer::external(cx, item.clone());
            js_buffer.upcast()
        }
        Element::Reference(reference) => {
            let js_array: Handle<JsArray> = cx.empty_array();

            for (index, bytes) in reference.iter().enumerate() {
                let js_buffer = JsBuffer::external(cx, bytes.clone());
                let js_value = js_buffer.as_value(cx);
                js_array.set(cx, index as u32, js_value)?;
            }

            js_array.upcast()
        }
        Element::Tree(tree) => {
            let js_buffer = JsBuffer::external(cx, tree.clone());
            js_buffer.upcast()
        }
    };

    js_object.set(cx, "value", js_value)?;
    NeonResult::Ok(js_object.upcast())
}

pub fn js_buffer_to_vec_u8<'a, C: Context<'a>>(js_buffer: Handle<JsBuffer>, cx: &mut C) -> Vec<u8> {
    let guard = cx.lock();
    // let key_buffer = js_buffer.deref();
    let key_memory_view = js_buffer.borrow(&guard);
    let key_slice = key_memory_view.as_slice::<u8>();
    key_slice.to_vec()
}

pub fn js_array_of_buffers_to_vec<'a, C: Context<'a>>(js_array: Handle<JsArray>, cx: &mut C) -> NeonResult<Vec<Vec<u8>>> {
    let buf_vec = js_array.to_vec(cx)?;
    let mut vec: Vec<Vec<u8>> = Vec::new();

    for buf in buf_vec {
        let js_buffer_handle = buf.downcast_or_throw::<JsBuffer, _>(cx)?;
        vec.push(js_buffer_to_vec_u8(js_buffer_handle, cx));
    }

    Ok(vec)
}
