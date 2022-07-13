use anyhow::{Context, Result};
use crossbeam_channel::Sender;
use events_api::Event;
use mq::*;
use proc_macro_utils::{Resource, RuntimeResource};
use runtime::resource::{get, Ctx, DataT, Linker, Map, Resource, ResourceMap, RuntimeResource};
use std::sync::{Arc, Mutex};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

wit_bindgen_wasmtime::export!("../../wit/mq.wit");
wit_error_rs::impl_error!(Error);
wit_error_rs::impl_from!(anyhow::Error, Error::ErrorWithDescription);
wit_error_rs::impl_from!(std::io::Error, Error::ErrorWithDescription);

const SCHEME_NAME: &str = "filemq";

/// A Filesystem implementation for the mq interface.
#[derive(Clone, Resource, RuntimeResource)]
pub struct MqFilesystem {
    queue: String,
    inner: Option<String>,
    host_state: Option<ResourceMap>,
}

// vvv we implement default manually because of the `queue` attribute
impl Default for MqFilesystem {
    fn default() -> Self {
        Self {
            queue: ".queue".to_string(),
            inner: Some(String::default()),
            host_state: None,
        }
    }
}

impl mq::Mq for MqFilesystem {
    /// Construct a new `MqFilesystem` instance provided a folder name. The folder will be created under `/tmp`.
    fn get_mq(&mut self, name: &str) -> Result<ResourceDescriptorResult, Error> {
        let path = Path::new("/tmp").join(name);
        let path = path
            .to_str()
            .with_context(|| format!("failed due to invalid path: {}", name))?
            .to_string();

        self.inner = Some(path);

        let rd = Uuid::new_v4().to_string();
        let cloned = self.clone();
        let mut map = Map::lock(&mut self.host_state)?;
        map.set(rd.clone(), (Box::new(cloned), None));
        Ok(rd)
    }

    /// Send a message to the message queue
    fn send(&mut self, rd: ResourceDescriptorParam, msg: PayloadParam<'_>) -> Result<(), Error> {
        Uuid::parse_str(rd).with_context(|| "failed to parse resource descriptor")?;

        let map = Map::lock(&mut self.host_state)?;
        let base = map.get::<String>(rd)?;

        // get a random name for a queue element
        let rand_file_name = format!(
            "{:?}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap()
        );

        fs::create_dir_all(&base)?;

        // create a file to store the queue element data
        let mut file = File::create(PathBuf::from(base).join(&rand_file_name))?;

        // write to file msg sent
        file.write_all(msg)?;

        // open/create queue and store one random name for a queue element per line
        let mut queue = fs::OpenOptions::new()
            .write(true)
            .append(true)
            .create(true)
            .open(PathBuf::from(base).join(&self.queue))?;

        // add queue element name to the bottom of the queue
        writeln!(queue, "{}", rand_file_name)?;

        Ok(())
    }

    /// Receive a message from the message queue
    fn receive(&mut self, rd: ResourceDescriptorParam) -> Result<PayloadResult, Error> {
        Uuid::parse_str(rd).with_context(|| "failed to parse resource descriptor")?;

        let map = Map::lock(&mut self.host_state)?;
        let base = map.get::<String>(rd)?;

        fs::create_dir_all(&base)?;

        // get the queue
        let queue = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(PathBuf::from(base).join(&self.queue))?;

        if queue.metadata().unwrap().len() != 0 {
            // get top element in the queue
            let mut queue_buffer = BufReader::new(&queue);
            let mut to_receive: String = String::from("");
            queue_buffer.read_line(&mut to_receive)?;

            // get queue after receive
            let mut queue_post_receive = queue_buffer
                .lines()
                .map(|x| x.unwrap())
                .collect::<Vec<String>>()
                .join("\n");

            // if queue isn't empty, we want to append a new-line char for subsequent send
            if !queue_post_receive.is_empty() {
                queue_post_receive += "\n";
            }

            // update queue status
            fs::write(PathBuf::from(base).join(&self.queue), queue_post_receive)?;

            // remove \n char from end of queue element
            to_receive.pop();

            // get element at top of queue
            let mut file = File::open(PathBuf::from(base).join(&to_receive))?;
            let mut buf = Vec::new();

            // read element's message
            file.read_to_end(&mut buf)?;

            // clean-up element from disk
            fs::remove_file(PathBuf::from(base).join(&to_receive))?;

            Ok(buf)
        } else {
            // if queue is empty, respond with empty string
            Ok(Vec::new())
        }
    }
}
