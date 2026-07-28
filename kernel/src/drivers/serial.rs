use core::fmt::{self, Write};

use lazy_static::lazy_static;
use uart_16550::SerialPort;

use crate::preemption::PreemptMutex;

const COM1_PORT: u16 = 0x3f8;

lazy_static! {
    static ref SERIAL1: PreemptMutex<SerialPort> = {
        // Outputs serial data to COM1
        let mut serial_port = unsafe { SerialPort::new(COM1_PORT) };
        serial_port.init();
        PreemptMutex::new(serial_port)
    };
}

#[doc(hidden)]
pub fn _print(arguments: fmt::Arguments<'_>) {
    SERIAL1
        .lock()
        .write_fmt(arguments)
        .expect("writing to COM1 failed");
}

#[macro_export]
macro_rules! serial_print {
    ($($argument:tt)*) => {
        $crate::serial::_print(format_args!($($argument)*))
    };
}

#[macro_export]
macro_rules! serial_println {
    () => {
        $crate::serial_print!("\n")
    };
    ($($argument:tt)*) => {
        $crate::serial_print!("{}\n", format_args!($($argument)*))
    };
}
