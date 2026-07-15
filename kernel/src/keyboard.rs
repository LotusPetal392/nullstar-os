use pc_keyboard::{DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1, layouts::Us104Key};
use spin::Mutex;

const SCANCODE_QUEUE_CAPACITY: usize = 128;

struct ScancodeQueue {
    bytes: [u8; SCANCODE_QUEUE_CAPACITY],
    read_index: usize,
    write_index: usize,
}

impl ScancodeQueue {
    const fn new() -> Self {
        Self {
            bytes: [0; SCANCODE_QUEUE_CAPACITY],
            read_index: 0,
            write_index: 0,
        }
    }

    fn push(&mut self, scancode: u8) {
        let next_write = (self.write_index + 1) % SCANCODE_QUEUE_CAPACITY;

        // Drop the newest scancode when the queue is full. This keeps the
        // interrupt handler bounded and avoids overwriting unread input.
        if next_write == self.read_index {
            return;
        }

        self.bytes[self.write_index] = scancode;
        self.write_index = next_write;
    }

    fn pop(&mut self) -> Option<u8> {
        if self.read_index == self.write_index {
            return None;
        }

        let scancode = self.bytes[self.read_index];
        self.read_index = (self.read_index + 1) % SCANCODE_QUEUE_CAPACITY;
        Some(scancode)
    }
}

static SCANCODES: Mutex<ScancodeQueue> = Mutex::new(ScancodeQueue::new());

static KEYBOARD: Mutex<PS2Keyboard<Us104Key, ScancodeSet1>> = Mutex::new(PS2Keyboard::new(
    ScancodeSet1::new(),
    Us104Key,
    HandleControl::Ignore,
));

pub(crate) fn push_scancode(scancode: u8) {
    SCANCODES.lock().push(scancode);
}

pub fn poll_key() -> Option<DecodedKey> {
    // IRQ1 also locks this queue, so prevent an interrupt from occurring
    // while the foreground code holds it.
    let scancode = x86_64::instructions::interrupts::without_interrupts(|| SCANCODES.lock().pop())?;

    let mut keyboard = KEYBOARD.lock();
    let event = keyboard.add_byte(scancode).ok().flatten()?;
    keyboard.process_keyevent(event)
}
