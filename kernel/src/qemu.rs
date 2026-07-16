use x86_64::instructions::{hlt, port::Port};

const QEMU_EXIT_PORT: u16 = 0xf4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum ExitCode {
    Success = 0x10,
    Failed = 0x11,
}

pub fn exit(code: ExitCode) -> ! {
    let mut port = Port::<u32>::new(QEMU_EXIT_PORT);

    unsafe {
        port.write(code as u32);
    }

    loop {
        hlt();
    }
}
