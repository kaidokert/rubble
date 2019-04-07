//! Utilities for decoding from and encoding into bytes.
//!
//! The most important parts offered by this module are [`ToBytes`] and [`FromBytes`],
//! general-purpose traits for zero-allocation (de)serialization from/to bytes. These are used all
//! over the place in Rubble, along with [`ByteWriter`]. In addition to those, this module also
//! defines helpful extension traits on `&[T]` and `&mut [T]`, defined on [`SliceExt`] and
//! [`MutSliceExt`], and on `&[u8]`, defined on [`BytesExt`].
//!
//! [`ToBytes`]: trait.ToBytes.html
//! [`FromBytes`]: trait.FromBytes.html
//! [`ByteWriter`]: struct.ByteWriter.html
//! [`SliceExt`]: trait.SliceExt.html
//! [`MutSliceExt`]: trait.MutSliceExt.html
//! [`BytesExt`]: trait.BytesExt.html

use {
    crate::ble::Error,
    byteorder::{ByteOrder, LittleEndian},
    core::{fmt, iter, mem},
};

/// Reference to a `T`, or to a byte slice that can be decoded as a `T`.
///
/// This type can be used in structures when storing a `T` directly is not desirable. For example,
/// `T` might actually be a slice `[U]` and thus has a non-static size, or `T`s size might simply be
/// too large (eg. it could be inside a rarely-encountered variant or would blow up the total size
/// of the containing enum).
///
/// The size of this type is currently 2 `usize`s plus a discriminant byte, but could potentially be
/// (unsafely) reduced further, should that be required.
pub struct BytesOr<'a, T: ?Sized>(Inner<'a, T>);

impl<'a, T: ?Sized> From<&'a T> for BytesOr<'a, T> {
    fn from(r: &'a T) -> Self {
        BytesOr(Inner::Or(r))
    }
}

enum Inner<'a, T: ?Sized> {
    Bytes(&'a [u8]),
    Or(&'a T),
}

impl<'a, T: ?Sized> Clone for Inner<'a, T> {
    fn clone(&self) -> Self {
        match self {
            Inner::Bytes(b) => Inner::Bytes(b),
            Inner::Or(t) => Inner::Or(t),
        }
    }
}

impl<'a, T: ?Sized> Clone for BytesOr<'a, T> {
    fn clone(&self) -> Self {
        BytesOr(self.0)
    }
}

impl<'a, T: ?Sized> Copy for BytesOr<'a, T> {}
impl<'a, T: ?Sized> Copy for Inner<'a, T> {}

impl<'a, T: fmt::Debug + FromBytes<'a> + Copy> fmt::Debug for BytesOr<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        self.read().fmt(f)
    }
}

impl<'a, T: fmt::Debug + FromBytes<'a> + Copy> fmt::Debug for BytesOr<'a, [T]> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<'a, T: ?Sized> BytesOr<'a, T> {
    /// Creates a `BytesOr` that holds on to a `T` via reference.
    ///
    /// For creating a `BytesOr` that references a byte slice, the [`FromBytes`] impl(s) can be
    /// used.
    ///
    /// [`FromBytes`]: trait.FromBytes.html
    pub fn from_ref(value: &'a T) -> Self {
        BytesOr(Inner::Or(value))
    }
}

/// Creates a `BytesOr` that stores bytes that can be decoded to a `T`.
///
/// This will check that `bytes` can indeed be decoded as a `T` using its [`FromBytes`]
/// implementation, and returns an error if not. An error will also be returned if `bytes` contains
/// more data than needed for a `T`.
///
/// [`FromBytes`]: trait.FromBytes.html
impl<'a, T: FromBytes<'a>> FromBytes<'a> for BytesOr<'a, T> {
    fn from_bytes(bytes: &mut ByteReader<'a>) -> Result<Self, Error> {
        // FIXME `bytes` should be advanced past the read bytes
        let raw = bytes.as_raw_bytes();

        {
            let mut bytes = ByteReader::new(raw);
            T::from_bytes(&mut bytes)?;
            if !bytes.is_empty() {
                // FIXME this is the only `FromBytes` impl that does something like this,
                // maybe it shouln't?
                return Err(Error::IncompleteParse);
            }
        }

        Ok(BytesOr(Inner::Bytes(raw)))
    }
}

/// Creates a `BytesOr` that stores bytes that can be decoded to a sequence of `T`s.
///
/// This will check that `bytes` can indeed be decoded as a sequence of `T`s, and returns an error
/// if not.
impl<'a, T: FromBytes<'a>> FromBytes<'a> for BytesOr<'a, [T]> {
    fn from_bytes(bytes: &mut ByteReader<'a>) -> Result<Self, Error> {
        let raw = bytes.as_raw_bytes();

        {
            let mut bytes = ByteReader::new(raw);
            while !bytes.is_empty() {
                T::from_bytes(&mut bytes)?;
            }
        }

        Ok(BytesOr(Inner::Bytes(raw)))
    }
}

impl<'a, T: ToBytes> ToBytes for BytesOr<'a, T> {
    fn to_bytes(&self, buffer: &mut ByteWriter) -> Result<(), Error> {
        match self.0 {
            Inner::Bytes(b) => buffer.write_slice(b),
            Inner::Or(t) => t.to_bytes(buffer),
        }
    }
}

impl<'a, T: ToBytes> ToBytes for BytesOr<'a, [T]> {
    fn to_bytes(&self, buffer: &mut ByteWriter) -> Result<(), Error> {
        match self.0 {
            Inner::Bytes(b) => buffer.write_slice(b),
            Inner::Or(ts) => {
                for t in ts {
                    t.to_bytes(buffer)?;
                }
                Ok(())
            }
        }
    }
}

impl<'a, T: Copy + FromBytes<'a>> BytesOr<'a, T> {
    /// Reads the `T`, possibly by parsing the stored bytes.
    ///
    /// If `self` already stores a reference to a `T`, the `T` will just be copied out. If `self`
    /// stores a byte slice, the `T` will be parsed using its [`FromBytes`] implementation.
    ///
    /// [`FromBytes`]: trait.FromBytes.html
    pub fn read(&self) -> T {
        match self.0 {
            Inner::Bytes(b) => {
                let mut bytes = ByteReader::new(b);
                let t = T::from_bytes(&mut bytes).unwrap();
                assert!(bytes.is_empty());
                t
            }
            Inner::Or(t) => *t,
        }
    }
}

impl<'a, T: Copy + FromBytes<'a>> BytesOr<'a, T> {
    /// Returns an iterator over all `T`s stored in `self` (which is just one `T` in this case).
    ///
    /// This method exists to mirror its twin implemented for `BytesOr<'a, [T]>`.
    pub fn iter(&self) -> impl Iterator<Item = T> + 'a {
        iter::once(self.read())
    }
}

impl<'a, T: Copy + FromBytes<'a>> BytesOr<'a, [T]> {
    /// Returns an iterator over all `T`s stored in `self`.
    ///
    /// The iterator will copy or decode `T`s out of `self`.
    pub fn iter(&self) -> impl Iterator<Item = T> + 'a {
        IterBytesOr { inner: *self }
    }
}

/// An iterator over values stored in a `BytesOr`.
struct IterBytesOr<'a, T> {
    inner: BytesOr<'a, [T]>,
}

impl<'a, T: Copy + FromBytes<'a>> Iterator for IterBytesOr<'a, T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner.0 {
            Inner::Bytes(b) => {
                if b.is_empty() {
                    None
                } else {
                    // Read a `T` and overwrite our `b` with the left-over data
                    let mut reader = ByteReader::new(*b);
                    let t = T::from_bytes(&mut reader).unwrap();
                    *b = reader.into_rest();
                    Some(t)
                }
            }
            Inner::Or(slice) => {
                let (first, rest) = slice.split_first()?;
                *slice = rest;
                Some(*first)
            }
        }
    }
}

/// Wrapper around a byte slice that can be used to encode data into bytes.
///
/// All `write_*` methods on this type will return `Error::Eof` when the underlying buffer slice is
/// full.
pub struct ByteWriter<'a>(&'a mut [u8]);

impl<'a> ByteWriter<'a> {
    /// Creates a writer that will write to `buf`.
    pub fn new(buf: &'a mut [u8]) -> Self {
        ByteWriter(buf)
    }

    /// Consumes `self` and returns the part of the contained buffer that has not yet been written
    /// to.
    pub fn into_rest(self) -> &'a mut [u8] {
        self.0
    }

    /// Returns the raw buffer this `ByteWriter` would write to.
    ///
    /// Combined with `skip`, this method allows advanced operations on the underlying byte buffer.
    pub fn rest(&mut self) -> &mut [u8] {
        self.0
    }

    /// Skips the given number of bytes in the output data without writing anything there.
    ///
    /// This is a potentially dangerous operation that should only be used when necessary (eg. when
    /// the skipped data will be filled in by other code). If the skipped bytes are *not* written,
    /// they will probably contain garbage data from an earlier use of the underlying buffer.
    pub fn skip(&mut self, bytes: usize) -> Result<(), Error> {
        if self.space_left() < bytes {
            Err(Error::Eof)
        } else {
            let this = mem::replace(&mut self.0, &mut []);
            self.0 = &mut this[bytes..];
            Ok(())
        }
    }

    /// Creates and returns another `ByteWriter` that can write to the next `len` Bytes in the
    /// buffer.
    ///
    /// `self` will be modified to point after the split-off bytes.
    ///
    /// Note that if the created `ByteWriter` is not used, the bytes will contain whatever contents
    /// they had before creating `self` (ie. most likely garbage data left over from earlier use).
    /// If you are really sure you want that, `skip` is a more explicit way of accomplishing that.
    #[must_use]
    pub fn split_off(&mut self, len: usize) -> Result<Self, Error> {
        if self.space_left() < len {
            Err(Error::Eof)
        } else {
            let this = mem::replace(&mut self.0, &mut []);
            let (head, tail) = this.split_at_mut(len);
            self.0 = tail;
            Ok(ByteWriter::new(head))
        }
    }

    /// Splits off the next byte in the buffer.
    ///
    /// The writer will be advanced to point to the rest of the underlying buffer.
    ///
    /// This allows filling in the value of the byte later, after writing more data.
    ///
    /// For a similar, but more flexible operation, see [`split_off`].
    ///
    /// [`split_off`]: #method.split_off
    pub fn split_next_mut(&mut self) -> Option<&'a mut u8> {
        let this = mem::replace(&mut self.0, &mut []);
        // Slight contortion to please the borrow checker:
        if this.is_empty() {
            self.0 = this;
            None
        } else {
            let (first, rest) = this.split_first_mut().unwrap();
            self.0 = rest;
            Some(first)
        }
    }

    /// Returns the number of bytes that can be written to `self` until it is full.
    pub fn space_left(&self) -> usize {
        self.0.len()
    }

    /// Writes all bytes from `other` to `self`.
    ///
    /// Returns `Error::Eof` when `self` does not have enough space left to fit `other`. In that
    /// case, `self` will not be modified.
    pub fn write_slice(&mut self, other: &[u8]) -> Result<(), Error> {
        if self.space_left() < other.len() {
            Err(Error::Eof)
        } else {
            self.0[..other.len()].copy_from_slice(other);
            let this = mem::replace(&mut self.0, &mut []);
            self.0 = &mut this[other.len()..];
            Ok(())
        }
    }

    /// Writes a single byte to `self`.
    ///
    /// Returns `Error::Eof` when no space is left.
    pub fn write_u8(&mut self, byte: u8) -> Result<(), Error> {
        let first = self.split_next_mut().ok_or(Error::Eof)?;
        *first = byte;
        Ok(())
    }

    /// Writes a `u16` to `self`, using Little Endian byte order.
    ///
    /// If `self` does not have enough space left, an error will be returned and no bytes will be
    /// written to `self`.
    pub fn write_u16_le(&mut self, value: u16) -> Result<(), Error> {
        let mut bytes = [0; 2];
        LittleEndian::write_u16(&mut bytes, value);
        self.write_slice(&bytes)
    }

    /// Writes a `u32` to `self`, using Little Endian byte order.
    ///
    /// If `self` does not have enough space left, an error will be returned and no bytes will be
    /// written to `self`.
    pub fn write_u32_le(&mut self, value: u32) -> Result<(), Error> {
        let mut bytes = [0; 4];
        LittleEndian::write_u32(&mut bytes, value);
        self.write_slice(&bytes)
    }

    /// Writes a `u64` to `self`, using Little Endian byte order.
    ///
    /// If `self` does not have enough space left, an error will be returned and no bytes will be
    /// written to `self`.
    pub fn write_u64_le(&mut self, value: u64) -> Result<(), Error> {
        let mut bytes = [0; 8];
        LittleEndian::write_u64(&mut bytes, value);
        self.write_slice(&bytes)
    }
}

/// Allows reading values from a borrowed byte slice.
pub struct ByteReader<'a>(&'a [u8]);

impl<'a> ByteReader<'a> {
    /// Creates a new `ByteReader` that will read from the given byte slice.
    pub fn new(bytes: &'a [u8]) -> Self {
        ByteReader(bytes)
    }

    /// Returns a reference to the raw bytes in `self`, without advancing `self` or reading any
    /// data.
    pub fn as_raw_bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Consumes `self` and returns the part of the contained buffer that has not yet been read
    /// from.
    pub fn into_rest(self) -> &'a [u8] {
        self.0
    }

    /// Skips the given number of bytes in the input data without inspecting them.
    ///
    /// This is a potentially dangerous operation that should only be used when the bytes really do
    /// not matter.
    pub fn skip(&mut self, bytes: usize) -> Result<(), Error> {
        if self.bytes_left() < bytes {
            Err(Error::Eof)
        } else {
            self.0 = &self.0[bytes..];
            Ok(())
        }
    }

    /// Creates and returns another `ByteReader` that will read from the next `len` Bytes in the
    /// buffer.
    ///
    /// `self` will be modified to point after the split-off bytes, and will continue reading from
    /// there.
    ///
    /// Note that if the created `ByteReader` is not used, the bytes will be ignored. If you are
    /// really sure you want that, `skip` is a more explicit way of accomplishing that.
    #[must_use]
    pub fn split_off(&mut self, len: usize) -> Result<Self, Error> {
        if self.bytes_left() < len {
            Err(Error::Eof)
        } else {
            let (head, tail) = (&self.0[..len], &self.0[len..]);
            self.0 = tail;
            Ok(ByteReader::new(head))
        }
    }

    /// Returns the number of bytes that can still be read from `self`.
    pub fn bytes_left(&self) -> usize {
        self.0.len()
    }

    /// Returns whether `self` is at the end of the underlying buffer (EOF).
    ///
    /// If this returns `true`, no data can be read from `self` anymore.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Reads a byte slice of length `len` from `self`.
    ///
    /// If `self` contains less than `len` bytes, `Error::Eof` will be returned and `self` will not
    /// be modified.
    pub fn read_slice(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if self.bytes_left() < len {
            Err(Error::Eof)
        } else {
            let slice = &self.0[..len];
            self.0 = &self.0[len..];
            Ok(slice)
        }
    }

    /// Reads a byte-array-like type `S` from `self`.
    ///
    /// `S` must implement `Default` and `AsMut<[u8]>`, which allows using small arrays up to 32
    /// bytes as well as datastructures from `alloc` (eg. `Box<[u8]>` or `Vec<u8>`).
    pub fn read_array<S>(&mut self) -> Result<S, Error>
    where
        S: Default + AsMut<[u8]>,
    {
        let mut buf = S::default();
        let slice = buf.as_mut();
        if self.bytes_left() < slice.len() {
            return Err(Error::Eof);
        }

        slice.copy_from_slice(&self.0[..slice.len()]);
        self.0 = &self.0[slice.len()..];
        Ok(buf)
    }

    /// Reads the remaining bytes from `self`.
    pub fn read_rest(&mut self) -> &'a [u8] {
        let rest = self.0;
        self.0 = &[];
        rest
    }

    /// Reads a single byte from `self`.
    ///
    /// Returns `Error::Eof` when `self` is empty.
    pub fn read_u8(&mut self) -> Result<u8, Error> {
        Ok(self.read_array::<[u8; 1]>()?[0])
    }

    /// Reads a `u16` from `self`, using Little Endian byte order.
    pub fn read_u16_le(&mut self) -> Result<u16, Error> {
        let arr = self.read_array::<[u8; 2]>()?;
        Ok(LittleEndian::read_u16(&arr))
    }

    /// Reads a `u32` from `self`, using Little Endian byte order.
    pub fn read_u32_le(&mut self) -> Result<u32, Error> {
        let arr = self.read_array::<[u8; 4]>()?;
        Ok(LittleEndian::read_u32(&arr))
    }

    /// Reads a `u64` from `self`, using Little Endian byte order.
    pub fn read_u64_le(&mut self) -> Result<u64, Error> {
        let arr = self.read_array::<[u8; 8]>()?;
        Ok(LittleEndian::read_u64(&arr))
    }
}

/// Trait for encoding a value into a byte buffer.
pub trait ToBytes {
    /// Converts `self` to bytes and writes them into `writer`, advancing `writer` to point past the
    /// encoded value.
    ///
    /// If `writer` does not contain enough space, an error will be returned and the state of the
    /// buffer is unspecified (eg. `self` may be partially written into `writer`).
    fn to_bytes(&self, writer: &mut ByteWriter) -> Result<(), Error>;
}

/// Trait for decoding values from a byte slice.
pub trait FromBytes<'a>: Sized {
    /// Decode a `Self` from a byte slice, advancing `bytes` to point past the data that was read.
    ///
    /// If `bytes` contains data not valid for the target type, or contains an insufficient number
    /// of bytes, an error will be returned and the state of `bytes` is unspecified (it can point to
    /// arbitrary data).
    fn from_bytes(bytes: &mut ByteReader<'a>) -> Result<Self, Error>;
}

impl ToBytes for [u8] {
    fn to_bytes(&self, writer: &mut ByteWriter) -> Result<(), Error> {
        writer.write_slice(self)
    }
}

impl<'a> ToBytes for &'a [u8] {
    fn to_bytes(&self, writer: &mut ByteWriter) -> Result<(), Error> {
        writer.write_slice(*self)
    }
}

impl<'a> FromBytes<'a> for &'a [u8] {
    fn from_bytes(bytes: &mut ByteReader<'a>) -> Result<Self, Error> {
        Ok(bytes.read_rest())
    }
}

impl<'a> FromBytes<'a> for u8 {
    fn from_bytes(bytes: &mut ByteReader<'a>) -> Result<Self, Error> {
        bytes.read_u8()
    }
}
