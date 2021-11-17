mod tests;

#[cfg(feature = "full")]
use backtrace::Backtrace;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt::Debug,
    marker::PhantomData,
};
#[cfg(feature = "full")]
use std::{
    error::Error,
    fs::File,
    io::{prelude::*, BufReader},
};
#[cfg(feature = "full")]
use tokio::{io::AsyncWriteExt, net::TcpSocket, runtime::Runtime};

pub trait Message: Sized {}
impl<T: Debug> Message for T {}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
/// The struct containing the formatted data received from the backtrace.
pub struct BacktraceData {
    pub name: String,
    pub file_path: String,
    pub line_number: u32,
    pub column_number: u32,
    pub code: String,
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
/// The main struct that is sent to the codeCTRL log server.
pub struct Log<T: Message> {
    /// A [`VecDeque`] of the [`BacktraceData`] generated by the
    /// [`log`] function.
    ///
    /// [`log`]: Self::log
    pub stack: VecDeque<BacktraceData>,
    /// The line number where the [`log`] function was called.
    ///
    /// [`log`]: Self::log
    pub line_number: u32,
    /// The code snippet around the line where the [`log`] function was called.
    ///
    /// [`log`]: Self::log
    pub code_snippet: BTreeMap<u32, String>,
    /// The message that the [`log`] function supplied.
    ///
    /// [`log`]: Self::log
    pub message: String,
    /// The message type that the [`log`] function supplied.
    ///
    /// [`log`]: Self::log
    pub message_type: String,
    /// The file name that the [`log`] function was called in.
    ///
    /// [`log`]: Self::log
    pub file_name: String,
    /// The IP address of the host that the [`log`] function was called on.
    ///
    /// [`log`]: Self::log
    pub address: String,
    #[serde(skip)]
    _t: PhantomData<T>,
}

#[cfg(feature = "full")]
impl<T: Message + Debug> Log<T> {
    pub fn log(
        message: T,
        surround: Option<u32>,
        port: Option<&str>,
    ) -> Result<(), Box<dyn Error>> {
        let mut log = Self {
            stack: VecDeque::new(),
            line_number: 0,
            file_name: String::new(),
            code_snippet: BTreeMap::new(),
            message: format!("{:#?}", &message),
            message_type: std::any::type_name::<T>().to_string(),
            address: String::new(),
            _t: PhantomData::<T>,
        };

        let surround = surround.unwrap_or(3);
        let port = port.unwrap_or("3001");

        log.get_stack_trace();

        if let Some(last) = log.stack.back() {
            log.code_snippet =
                Self::get_code_snippet(&last.file_path, last.line_number, surround);
            log.line_number = last.line_number;

            log.file_name = last
                .file_path
                .split('/')
                .skip_while(|s| *s != "src")
                .collect::<Vec<_>>()
                .join("/");
        }

        let rt = Runtime::new()?;
        let mut ret = Ok(());

        rt.block_on(async {
            ret = Self::_log(&mut log, port).await;
        });

        ret
    }

    // We have a non-async wrapper over _log so that we can log from non-async
    // scopes.
    //
    // TODO: Provide a direct wrapper so that async environments do not need to call
    // a non-async wrapper, just for that to call an async wrapper.
    async fn _log(log: &mut Self, port: &str) -> Result<(), Box<dyn Error>> {
        let socket = TcpSocket::new_v4()?;
        let mut stream = socket
            .connect(format!("127.0.0.1:{}", port).parse().unwrap())
            .await?;

        let data = serde_cbor::to_vec(log)?;

        stream.write_all(&data).await?;

        Ok(())
    }

    fn get_stack_trace(&mut self) {
        let backtrace = Backtrace::new();

        for frame in backtrace.frames() {
            backtrace::resolve(frame.ip(), |symbol| {
                let name = if let Some(symbol) = symbol.name() {
                    let mut symbol = symbol.to_string();
                    let mut split = symbol.split("::").collect::<Vec<&str>>();

                    if split.len() > 1 {
                        split.remove(split.len() - 1);
                    }

                    symbol = split.join("::");

                    symbol
                } else {
                    "".into()
                };

                if let (Some(file_name), Some(line_number), Some(column_number)) =
                    (symbol.filename(), symbol.lineno(), symbol.colno())
                {
                    let file_path: String =
                        file_name.as_os_str().to_str().unwrap().to_string();

                    if !name.ends_with("Log<T>::log")
                        && !name.ends_with("Log<T>::get_stack_trace")
                        && !file_path.starts_with("/rustc/")
                    {
                        let code = Self::get_code(&file_path, line_number);

                        self.stack.push_front(BacktraceData {
                            name,
                            file_path,
                            line_number,
                            column_number,
                            code,
                        });
                    }
                }
            });
        }
    }

    fn get_code(file_path: &str, line_number: u32) -> String {
        let mut code = String::new();

        let file = File::open(file_path).unwrap_or_else(|_| {
            panic!("Unexpected error: could not open file: {}", file_path)
        });

        let reader = BufReader::new(file);

        if let Some(Ok(line)) = reader.lines().nth(line_number.saturating_sub(1) as usize)
        {
            code = line.trim().to_string();
        }

        code
    }

    fn get_code_snippet(
        file_path: &str,
        line_number: u32,
        surround: u32,
    ) -> BTreeMap<u32, String> {
        let file = File::open(file_path).unwrap_or_else(|_| {
            panic!("Unexpected error: could not open file: {}", file_path)
        });

        let offset = line_number.saturating_sub(surround);
        let reader = BufReader::new(file);

        let lines: BTreeMap<u32, String> = reader
            .lines()
            .enumerate()
            .filter(|(_, line)| line.is_ok())
            .map(|(n, line)| ((n + 1) as u32, line.unwrap()))
            .collect();

        let mut end = line_number.saturating_add(surround);

        if end > lines.len() as u32 - 1 {
            end = lines.len() as u32 - 1;
        }

        lines
            .range(offset..=end)
            .map(|(key, value)| (*key, value.clone()))
            .collect()
    }
}