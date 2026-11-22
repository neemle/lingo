use std::io::{self, Read, Write};

pub(crate) struct LimitedReader<R> {
    inner: R,
    remaining: u64,
}

impl<R> LimitedReader<R> {
    pub(crate) fn new(inner: R, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut extra = [0];
            return match self.inner.read(&mut extra)? {
                0 => Ok(0),
                _ => Err(io::Error::other("Резервна копія перевищує 1 ГіБ.")),
            };
        }
        let count = output.len().min(self.remaining as usize);
        let read = self.inner.read(&mut output[..count])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

pub(crate) struct LimitedWriter<W> {
    inner: W,
    remaining: u64,
}

impl<W> LimitedWriter<W> {
    pub(crate) fn new(inner: W, limit: u64) -> Self {
        Self {
            inner,
            remaining: limit,
        }
    }
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        if input.len() as u64 > self.remaining {
            return Err(io::Error::other("Резервна копія перевищує 1 ГіБ."));
        }
        let written = self.inner.write(input)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_io_without_accepting_truncation_or_writing_past_limit() {
        let mut output = Vec::new();
        let mut writer = LimitedWriter::new(&mut output, 3);
        writer.write_all(b"abc").unwrap();
        assert!(writer.write_all(b"d").is_err());
        assert_eq!(output, b"abc");
        let mut bytes = Vec::new();
        assert!(LimitedReader::new(&b"abcd"[..], 3)
            .read_to_end(&mut bytes)
            .is_err());
        assert_eq!(bytes, b"abc");
        let mut exact = Vec::new();
        LimitedReader::new(&b"abc"[..], 3)
            .read_to_end(&mut exact)
            .unwrap();
        assert_eq!(exact, b"abc");
    }
}
