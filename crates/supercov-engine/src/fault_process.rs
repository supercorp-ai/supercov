//! Starting a program, where a test can make it fail: with [`Launch`] in
//! scope, `spawn#N` fails the first start from the Nth place in the code
//! that starts one (see [`crate::fault`]), so what each caller does when a
//! program cannot be started runs under test.

use std::{
    io,
    process::{Child, Command, ExitStatus, Output},
};

/// [`Command`]'s `spawn`, `output` and `status`, each a fault site at the
/// place it is called from.
pub trait Launch {
    fn launch(&mut self) -> io::Result<Child>;
    fn launch_output(&mut self) -> io::Result<Output>;
    fn launch_status(&mut self) -> io::Result<ExitStatus>;
}

impl Launch for Command {
    #[track_caller]
    fn launch(&mut self) -> io::Result<Child> {
        crate::fault::io_here("spawn", "spawn", io::ErrorKind::NotFound, || self.spawn())
    }

    #[track_caller]
    fn launch_output(&mut self) -> io::Result<Output> {
        crate::fault::io_here("spawn", "output", io::ErrorKind::NotFound, || self.output())
    }

    #[track_caller]
    fn launch_status(&mut self) -> io::Result<ExitStatus> {
        crate::fault::io_here("spawn", "status", io::ErrorKind::NotFound, || self.status())
    }
}
