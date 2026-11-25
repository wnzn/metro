use std::io::ErrorKind;

use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;

pub(super) async fn send_stdin(mut stdin: ChildStdin, input: Option<&str>) {
    if let Some(input) = input {
        match stdin.write_all(input.as_bytes()).await {
            // The CLI can reject arguments before reading stdin. The caller
            // still checks the child's exit status and output.
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
            Err(e) => panic!("could not write connector stdin: {e}"),
        }
    }
}
