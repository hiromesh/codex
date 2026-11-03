use codex_exec_server_protocol::JSONRPCErrorError;
use codex_sandboxing::SandboxExecRequest;
use codex_utils_path_uri::PathUri;
use tokio::io;

use crate::fs_helper::FsHelperOpenParams;
use crate::fs_helper::FsHelperOpenResponse;
use crate::fs_helper::FsHelperPayload;
use crate::fs_helper::FsHelperRequest;
use crate::fs_helper::FsHelperResponse;
use crate::fs_sandbox::io_error;
use crate::fs_sandbox::spawn_command;
#[cfg(unix)]
use crate::fs_sandbox::wait_for_helper_output;
use crate::protocol::FsOpenMode;
use crate::rpc::internal_error;
use crate::rpc::invalid_request;

pub(crate) async fn open(
    command: SandboxExecRequest,
    path: PathUri,
    mode: FsOpenMode,
) -> Result<tokio::fs::File, JSONRPCErrorError> {
    let request = serde_json::to_vec(&FsHelperRequest::Open(FsHelperOpenParams { path, mode }))
        .map_err(|error| internal_error(format!("invalid fs sandbox helper request: {error}")))?;
    open_platform(command, request).await
}

fn open_response(response: &[u8]) -> Result<FsHelperOpenResponse, JSONRPCErrorError> {
    match serde_json::from_slice(response).map_err(|error| {
        internal_error(format!("invalid fs sandbox helper open response: {error}"))
    })? {
        FsHelperResponse::Ok(FsHelperPayload::Open(response)) => Ok(response),
        FsHelperResponse::Ok(_) => Err(invalid_request(
            "invalid fs sandbox helper open response".to_string(),
        )),
        FsHelperResponse::Error(error) => Err(error),
    }
}

// Unix passes the opened fd over the helper's stdin socket.
#[cfg(unix)]
async fn open_platform(
    command: SandboxExecRequest,
    request: Vec<u8>,
) -> Result<tokio::fs::File, JSONRPCErrorError> {
    use std::io::Write;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;

    let (mut receiver, sender) = UnixStream::pair().map_err(io_error)?;
    let sender: OwnedFd = sender.into();
    let child = spawn_command(command, codex_utils_pty::ChildStdin::File(sender))?;
    receiver.write_all(&request).map_err(io_error)?;
    receiver
        .shutdown(std::net::Shutdown::Write)
        .map_err(io_error)?;

    let output = wait_for_helper_output(child).await?;
    open_response(&output.stdout)?;
    let descriptor = receive_file_descriptor(&receiver).map_err(io_error)?;
    Ok(tokio::fs::File::from_std(std::fs::File::from(descriptor)))
}

// SCM_RIGHTS is Unix-only.
#[cfg(unix)]
pub(crate) fn transfer_file(file: &tokio::fs::File) -> io::Result<()> {
    use rustix::net::SendAncillaryBuffer;
    use rustix::net::SendAncillaryMessage;
    use rustix::net::SendFlags;
    use std::io::IoSlice;
    use std::os::fd::AsFd;

    let descriptors = [file.as_fd()];
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if !control.push(SendAncillaryMessage::ScmRights(&descriptors)) {
        return Err(io::Error::other("missing file-descriptor control header"));
    }
    if rustix::net::sendmsg(
        std::io::stdin(),
        &[IoSlice::new(&[0])],
        &mut control,
        SendFlags::empty(),
    )? != 1
    {
        return Err(io::Error::other(
            "fs sandbox helper did not transfer its opened file descriptor",
        ));
    }
    Ok(())
}

// File-descriptor passing is only available on Unix.
#[cfg(unix)]
fn receive_file_descriptor(
    socket: &std::os::unix::net::UnixStream,
) -> io::Result<std::os::fd::OwnedFd> {
    use rustix::net::RecvAncillaryBuffer;
    use rustix::net::RecvAncillaryMessage;
    use rustix::net::RecvFlags;
    use rustix::net::ReturnFlags;
    use std::io::IoSliceMut;

    let mut byte = [0_u8];
    let mut buffers = [IoSliceMut::new(&mut byte)];
    let mut space = [std::mem::MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    // Linux can set close-on-exec while receiving the fd.
    #[cfg(target_os = "linux")]
    let flags = RecvFlags::CMSG_CLOEXEC;
    // Other Unix platforms need the non-atomic fcntl call below.
    #[cfg(not(target_os = "linux"))]
    let flags = RecvFlags::empty();
    let message = rustix::net::recvmsg(socket, &mut buffers, &mut control, flags)?;
    if message.bytes != 1 || message.flags.contains(ReturnFlags::CTRUNC) {
        return Err(io::Error::other("invalid file-descriptor control message"));
    }
    let descriptor = control
        .drain()
        .find_map(|message| match message {
            RecvAncillaryMessage::ScmRights(mut descriptors) => descriptors.next(),
            _ => None,
        })
        .ok_or_else(|| io::Error::other("missing transferred file descriptor"))?;
    // macOS cannot set this atomically, so the fd is briefly inheritable.
    rustix::io::fcntl_setfd(&descriptor, rustix::io::FdFlags::CLOEXEC)?;
    Ok(descriptor)
}
