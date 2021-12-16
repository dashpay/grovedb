mod converter;

use std::path::Path;
use std::sync::mpsc;
use std::thread;

use grovedb::{GroveDb};
use neon::prelude::*;

type DbCallback = Box<dyn FnOnce(&mut GroveDb, &Channel) + Send>;
type CloseCallback = Box<dyn FnOnce(&Channel) + Send>;

// Messages sent on the database channel
enum DbMessage {
    // Callback to be executed
    Callback(DbCallback),
    // Indicates that the thread should be stopped and connection closed
    Close(CloseCallback),
}

struct GroveDbWrapper {
    tx: mpsc::Sender<DbMessage>,
}

// Internal wrapper logic. Needed to avoid issues with passing threads to node.js.
// Avoid thread conflicts bu having a dedicated thread thread for the groveDB
// and uses event to communicate with it
impl GroveDbWrapper {
    // Creates a new instance of `GroveDbWrapper`
    //
    // 1. Creates a connection and a channel
    // 2. Spawns a thread and moves the channel receiver and connection to it
    // 3. On a separate thread, read closures off the channel and execute with access
    //    to the connection.
    fn new(cx: &mut FunctionContext) -> NeonResult<Self>
    {
        // TODO: error handling
        let path_string = cx.argument::<JsString>(0)?.value(cx);

        // Channel for sending callbacks to execute on the GroveDb connection thread
        let (tx, rx) = mpsc::channel::<DbMessage>();

        // Create an `Channel` for calling back to JavaScript. It is more efficient
        // to create a single channel and re-use it for all database callbacks.
        // The JavaScript process will not exit as long as this channel has not been
        // dropped.
        let channel = cx.channel();

        // Spawn a thread for processing database queries
        // This will not block the JavaScript main thread and will continue executing
        // concurrently.
        thread::spawn(move || {
            let path = Path::new(&path_string);
            // Open a connection to groveDb, this will be moved to a separate thread
            // TODO: think how to pass this error to JS
            let mut grove_db = GroveDb::open(path).unwrap();

            // Blocks until a callback is available
            // When the instance of `Database` is dropped, the channel will be closed
            // and `rx.recv()` will return an `Err`, ending the loop and terminating
            // the thread.
            while let Ok(message) = rx.recv() {
                match message {
                    DbMessage::Callback(callback) => {
                        // The connection and channel are owned by the thread, but _lent_ to
                        // the callback. The callback has exclusive access to the connection
                        // for the duration of the callback.
                        callback(&mut grove_db, &channel);
                    }
                    // Immediately close the connection, even if there are pending messages
                    DbMessage::Close(callback) => {
                        callback(&channel);
                        break;
                    }
                }
            }
        });

        Ok(Self { tx })
    }

    // Idiomatic rust would take an owned `self` to prevent use after close
    // However, it's not possible to prevent JavaScript from continuing to hold a closed database
    fn close(
        &self,
        callback: impl FnOnce(&Channel) + Send + 'static,
    ) -> Result<(), mpsc::SendError<DbMessage>> {
        self.tx.send(DbMessage::Close(Box::new(callback)))
    }

    fn send_to_db_thread(
        &self,
        callback: impl FnOnce(&mut GroveDb, &Channel) + Send + 'static,
    ) -> Result<(), mpsc::SendError<DbMessage>> {
        self.tx.send(DbMessage::Callback(Box::new(callback)))
    }
}

// Ensures that GroveDbWrapper is properly disposed when the corresponding JS object
// gets garbage collected
impl Finalize for GroveDbWrapper {}

// External wrapper logic
impl GroveDbWrapper {
    // Create a new instance of `Database` and place it inside a `JsBox`
    // JavaScript can hold a reference to a `JsBox`, but the contents are opaque
    fn js_open(mut cx: FunctionContext) -> JsResult<JsBox<GroveDbWrapper>> {
        let grove_db_wrapper =
            GroveDbWrapper::new(&mut cx).or_else(|err| cx.throw_error(err.to_string()))?;

        Ok(cx.boxed(grove_db_wrapper))
    }

    fn js_get(mut cx: FunctionContext) -> JsResult<JsUndefined> {
        let js_path = cx.argument::<JsArray>(0)?;
        let js_key = cx.argument::<JsBuffer>(1)?;
        let js_callback = cx.argument::<JsFunction>(2)?.root(&mut cx);

        let path = converter::js_array_of_buffers_to_vec(js_path, &mut cx)?;
        let key = converter::js_buffer_to_vec_u8(js_key, &mut cx);

        // Get the `this` value as a `JsBox<Database>`
        let db = cx
            .this()
            .downcast_or_throw::<JsBox<GroveDbWrapper>, _>(&mut cx)?;

        db.send_to_db_thread(move |grove_db: &mut GroveDb, channel| {
            let path_slice: Vec<&[u8]> = path.iter().map(|fragment| fragment.as_slice()).collect();
            let result = grove_db.get(&path_slice, &key);

            channel.send(move |mut task_context| {
                let callback = js_callback.into_inner(&mut task_context);
                let this = task_context.undefined();
                let callback_arguments: Vec<Handle<JsValue>> = match result {
                    // Convert the name to a `JsString` on success and upcast to a `JsValue`
                    Ok(element) => {
                        // First parameter of JS callbacks is error, which is null in this case
                        vec![
                            task_context.null().upcast(),
                            converter::element_to_js_object(element, &mut task_context)?
                        ]
                    },

                    // Convert the error to a JavaScript exception on failure
                    Err(err) => vec![
                        task_context.error(err.to_string())?.upcast()
                    ],
                };

                callback.call(&mut task_context, this, callback_arguments)?;

                Ok(())
            });
        })
        .or_else(|err| cx.throw_error(err.to_string()))?;

        // The result is returned through the callback, not through direct return
        Ok(cx.undefined())
    }

    fn js_insert(mut cx: FunctionContext) -> JsResult<JsUndefined> {
        let js_path = cx.argument::<JsArray>(0)?;
        let js_key = cx.argument::<JsBuffer>(1)?;
        let js_element = cx.argument::<JsObject>(2)?;
        let js_callback = cx.argument::<JsFunction>(3)?.root(&mut cx);

        let path = converter::js_array_of_buffers_to_vec(js_path, &mut cx)?;
        let key = converter::js_buffer_to_vec_u8(js_key, &mut cx);
        let element = converter::js_object_to_element(js_element, &mut cx)?;

        // Get the `this` value as a `JsBox<Database>`
        let db = cx
            .this()
            .downcast_or_throw::<JsBox<GroveDbWrapper>, _>(&mut cx)?;

        db.send_to_db_thread(move |grove_db: &mut GroveDb, channel| {
            let path_slice: Vec<&[u8]> = path.iter().map(|fragment| fragment.as_slice()).collect();
            let result = grove_db.insert(&path_slice, key, element);

            channel.send(move |mut task_context| {
                let callback = js_callback.into_inner(&mut task_context);
                let this = task_context.undefined();
                let callback_arguments: Vec<Handle<JsValue>> = match result {
                    Ok(_) => vec![task_context.null().upcast()],
                    Err(err) => vec![task_context.error(err.to_string())?.upcast()],
                };

                callback.call(&mut task_context, this, callback_arguments)?;
                Ok(())
            });
        })
            .or_else(|err| cx.throw_error(err.to_string()))?;

        Ok(cx.undefined())
    }

    /// Not implemented
    fn js_proof(mut cx: FunctionContext) -> JsResult<JsUndefined> {
        Ok(cx.undefined())
    }

    /// Sends a message to the DB thread to stop the thread and dispose the
    /// groveDb instance owned by it, then calls js callback passed as a first
    /// argument to the function
    fn js_close(mut cx: FunctionContext) -> JsResult<JsUndefined> {
        let js_callback = cx.argument::<JsFunction>(0)?.root(&mut cx);

        let db = cx
            .this()
            .downcast_or_throw::<JsBox<GroveDbWrapper>, _>(&mut cx)?;

        db.close(|channel| {
            channel.send(move |mut task_context| {
                let callback = js_callback.into_inner(&mut task_context);
                let this = task_context.undefined();
                let callback_arguments: Vec<Handle<JsValue>> = vec![
                    task_context.null().upcast(),
                ];

                callback.call(&mut task_context, this, callback_arguments)?;

                Ok(())
            });
        }).or_else(|err| cx.throw_error(err.to_string()))?;

        Ok(cx.undefined())
    }
}

#[neon::main]
fn main(mut cx: ModuleContext) -> NeonResult<()> {
    cx.export_function("groveDbOpen", GroveDbWrapper::js_open)?;
    cx.export_function("groveDbInsert", GroveDbWrapper::js_insert)?;
    cx.export_function("groveDbGet", GroveDbWrapper::js_get)?;
    cx.export_function("groveDbProof", GroveDbWrapper::js_proof)?;
    cx.export_function("groveDbClose", GroveDbWrapper::js_close)?;

    Ok(())
}
