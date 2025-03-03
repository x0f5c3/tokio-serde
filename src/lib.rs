//! This crate provides the utilities needed to easily implement a Tokio
//! transport using [serde] for serialization and deserialization of frame
//! values.
//!
//! # Introduction
//!
//! This crate provides [transport] combinators that transform a stream of
//! frames encoded as bytes into a stream of frame values. It is expected that
//! the framing happens at another layer. One option is to use a [length
//! delimited] framing transport.
//!
//! The crate provides two traits that must be implemented: [`Serializer`] and
//! [`Deserializer`]. Implementations of these traits are then passed to
//! [`Framed`] along with the upstream [`Stream`] or
//! [`Sink`] that handles the byte encoded frames.
//!
//! By doing this, a transformation pipeline is built. For reading, it looks
//! something like this:
//!
//! * `tokio_serde::Framed`
//! * `tokio_util::codec::FramedRead`
//! * `tokio::net::TcpStream`
//!
//! The write half looks like:
//!
//! * `tokio_serde::Framed`
//! * `tokio_util::codec::FramedWrite`
//! * `tokio::net::TcpStream`
//!
//! # Examples
//!
//! For an example, see how JSON support is implemented:
//!
//! * [server](https://github.com/carllerche/tokio-serde/blob/master/examples/server.rs)
//! * [client](https://github.com/carllerche/tokio-serde/blob/master/examples/client.rs)
//!
//! [serde]: https://serde.rs
//! [serde-json]: https://github.com/serde-rs/json
//! [transport]: https://tokio.rs/docs/going-deeper/transports/
//! [length delimited]: https://docs.rs/tokio-util/0.2/tokio_util/codec/length_delimited/index.html
//! [`Serializer`]: trait.Serializer.html
//! [`Deserializer`]: trait.Deserializer.html
//! [`Framed`]: struct.Framed.html
//! [`Stream`]: https://docs.rs/futures/0.3/futures/stream/trait.Stream.html
//! [`Sink`]: https://docs.rs/futures/0.3/futures/sink/trait.Sink.html

#![cfg_attr(docsrs, feature(doc_cfg))]

extern crate core;

use bytes::{Bytes, BytesMut};
use futures_core::{ready, Stream, TryStream};
use futures_sink::Sink;
use pin_project::pin_project;
use std::{
    marker::PhantomData,
    pin::Pin,
    task::{Context, Poll},
};

/// Serializes a value into a destination buffer
///
/// Implementations of `Serializer` are able to take values of type `T` and
/// convert them to a byte representation. The specific byte format, i.e. JSON,
/// protobuf, binpack, ... is an implementation detail.
///
/// The `serialize` function takes `&mut self`, allowing for `Serializer`
/// instances to be created with runtime configuration settings.
///
/// # Examples
///
/// An integer serializer that allows the width to be configured.
///
/// ```
/// use tokio_serde::Serializer;
/// use bytes::{Buf, Bytes, BytesMut, BufMut};
/// use std::pin::Pin;
/// use bincode_crate::Options;
///
/// struct IntSerializer {
///     width: usize,
/// }
///
/// #[derive(Debug)]
/// enum Error {
///     Overflow,
/// }
///
/// impl Serializer<u64> for IntSerializer {
///     type Error = Error;
///
///     fn serialize(self: Pin<&mut Self>, item: &u64) -> Result<Bytes, Self::Error> {
///         assert!(self.width <= 8);
///
///         let max = (1 << (self.width * 8)) - 1;
///
///         if *item > max {
///             return Err(Error::Overflow);
///         }
///
///         let mut ret = BytesMut::with_capacity(self.width);
///         ret.put_uint(*item, self.width);
///         Ok(ret.into())
///     }
/// }
///
/// let mut serializer = IntSerializer { width: 3 };
///
/// let buf = Pin::new(&mut serializer).serialize(&5).unwrap();
/// assert_eq!(buf, &b"\x00\x00\x05"[..]);
/// ```
pub trait Serializer<T> {
    type Error;

    /// Serializes `item` into a new buffer
    ///
    /// The serialization format is specific to the various implementations of
    /// `Serializer`. If the serialization is successful, a buffer containing
    /// the serialized item is returned. If the serialization is unsuccessful,
    /// an error is returned.
    ///
    /// Implementations of this function should not mutate `item` via any sort
    /// of internal mutability strategy.
    ///
    /// See the trait level docs for more detail.
    fn serialize(self: Pin<&mut Self>, item: &T) -> Result<Bytes, Self::Error>;
}

/// Deserializes a value from a source buffer
///
/// Implementatinos of `Deserializer` take a byte buffer and return a value by
/// parsing the contents of the buffer according to the implementation's format.
/// The specific byte format, i.e. JSON, protobuf, binpack, is an implementation
/// detail
///
/// The `deserialize` function takes `&mut self`, allowing for `Deserializer`
/// instances to be created with runtime configuration settings.
///
/// It is expected that the supplied buffer represents a full value and only
/// that value. If after deserializing a value there are remaining bytes the
/// buffer, the deserializer will return an error.
///
/// # Examples
///
/// An integer deserializer that allows the width to be configured.
///
/// ```
/// use tokio_serde::Deserializer;
/// use bytes::{BytesMut, Buf};
/// use std::pin::Pin;
/// use bincode_crate::Options;
///
/// struct IntDeserializer {
///     width: usize,
/// }
///
/// #[derive(Debug)]
/// enum Error {
///     Underflow,
///     Overflow
/// }
///
/// impl Deserializer<u64> for IntDeserializer {
///     type Error = Error;
///
///     fn deserialize(self: Pin<&mut Self>, buf: &BytesMut) -> Result<u64, Self::Error> {
///         assert!(self.width <= 8);
///
///         if buf.len() > self.width {
///             return Err(Error::Overflow);
///         }
///
///         if buf.len() < self.width {
///             return Err(Error::Underflow);
///         }
///
///         let ret = std::io::Cursor::new(buf).get_uint(self.width);
///         Ok(ret)
///     }
/// }
///
/// let mut deserializer = IntDeserializer { width: 3 };
///
/// let i = Pin::new(&mut deserializer).deserialize(&b"\x00\x00\x05"[..].into()).unwrap();
/// assert_eq!(i, 5);
/// ```
pub trait Deserializer<T> {
    type Error;

    /// Deserializes a value from `buf`
    ///
    /// The serialization format is specific to the various implementations of
    /// `Deserializer`. If the deserialization is successful, the value is
    /// returned. If the deserialization is unsuccessful, an error is returned.
    ///
    /// See the trait level docs for more detail.
    fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<T, Self::Error>;
}

/// Adapts a transport to a value sink by serializing the values and to a stream of values by deserializing them.
///
/// It is expected that the buffers yielded by the supplied transport be framed. In
/// other words, each yielded buffer must represent exactly one serialized
/// value.
///
/// The provided transport will receive buffer values containing the
/// serialized value. Each buffer contains exactly one value. This sink will be
/// responsible for writing these buffers to an `AsyncWrite` using some sort of
/// framing strategy.
///
/// The specific framing strategy is left up to the
/// implementor. One option would be to use [length_delimited] provided by
/// [tokio-util].
///
/// [length_delimited]: http://docs.rs/tokio-util/0.2/tokio_util/codec/length_delimited/index.html
/// [tokio-util]: http://crates.io/crates/tokio-util
#[pin_project]
#[derive(Debug)]
pub struct Framed<Transport, Item, SinkItem, Codec> {
    #[pin]
    inner: Transport,
    #[pin]
    codec: Codec,
    item: PhantomData<(Item, SinkItem)>,
}

impl<Transport, Item, SinkItem, Codec> Framed<Transport, Item, SinkItem, Codec> {
    /// Creates a new `Framed` with the given transport and codec.
    pub fn new(inner: Transport, codec: Codec) -> Self {
        Self {
            inner,
            codec,
            item: PhantomData,
        }
    }

    /// Returns a reference to the underlying transport wrapped by `Framed`.
    ///
    /// Note that care should be taken to not tamper with the underlying transport as
    /// it may corrupt the sequence of frames otherwise being worked with.
    pub fn get_ref(&self) -> &Transport {
        &self.inner
    }

    /// Returns a mutable reference to the underlying transport wrapped by
    /// `Framed`.
    ///
    /// Note that care should be taken to not tamper with the underlying transport as
    /// it may corrupt the sequence of frames otherwise being worked with.
    pub fn get_mut(&mut self) -> &mut Transport {
        &mut self.inner
    }

    /// Consumes the `Framed`, returning its underlying transport.
    ///
    /// Note that care should be taken to not tamper with the underlying transport as
    /// it may corrupt the sequence of frames otherwise being worked with.
    pub fn into_inner(self) -> Transport {
        self.inner
    }
}

impl<Transport, Item, SinkItem, Codec> Stream for Framed<Transport, Item, SinkItem, Codec>
where
    Transport: TryStream<Ok = BytesMut>,
    Transport::Error: From<Codec::Error>,
    BytesMut: From<Transport::Ok>,
    Codec: Deserializer<Item>,
{
    type Item = Result<Item, Transport::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match ready!(self.as_mut().project().inner.try_poll_next(cx)) {
            Some(bytes) => Poll::Ready(Some(Ok(self
                .as_mut()
                .project()
                .codec
                .deserialize(&bytes?)?))),
            None => Poll::Ready(None),
        }
    }
}

impl<Transport, Item, SinkItem, Codec> Sink<SinkItem> for Framed<Transport, Item, SinkItem, Codec>
where
    Transport: Sink<Bytes>,
    Codec: Serializer<SinkItem>,
    Codec::Error: Into<Transport::Error>,
{
    type Error = Transport::Error;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: SinkItem) -> Result<(), Self::Error> {
        let res = self.as_mut().project().codec.serialize(&item);
        let bytes = res.map_err(Into::into)?;

        self.as_mut().project().inner.start_send(bytes)?;

        Ok(())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        ready!(self.as_mut().poll_flush(cx))?;
        self.project().inner.poll_close(cx)
    }
}

pub type SymmetricallyFramed<Transport, Value, Codec> = Framed<Transport, Value, Value, Codec>;

#[cfg(any(
    feature = "json",
    feature = "bincode",
    feature = "messagepack",
    feature = "cbor",
    feature = "encrypted_bincode"
))]
pub mod formats {
    #[cfg(feature = "bincode")]
    pub use self::bincode::*;
    #[cfg(feature = "cbor")]
    pub use self::cbor::*;
    #[cfg(feature = "encrypted_bincode")]
    pub use self::encrypted_bincode::*;
    #[cfg(feature = "json")]
    pub use self::json::*;
    #[cfg(feature = "messagepack")]
    pub use self::messagepack::*;

    use super::{Deserializer, Serializer};
    use bytes::{Bytes, BytesMut};
    use educe::Educe;
    pub(crate) use serde::{Deserialize, Serialize};
    use std::{marker::PhantomData, pin::Pin};

    #[cfg(feature = "encrypted_bincode")]
    mod encrypted_bincode {
        use super::*;
        use bincode_crate::config::Options;
        use chacha20poly1305::aead::rand_core::{OsRng, RngCore};
        use chacha20poly1305::aead::{Aead, NewAead};
        use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
        use std::io;
        use std::io::ErrorKind;
        use secrecy::{ExposeSecret, Secret};

        fn gen_key() -> Secret<Vec<u8>> {
            let mut res = Key::default();
            let mut rng = OsRng::default();
            rng.fill_bytes(&mut res);
            Secret::new(res.to_vec())
        }
        /// Encrypted bincode codec using [bincode](https://docs.rs/bincode) crate
        /// for serialization and [chacha20poly1305](https://docs.rs/chacha20poly1305) for encryption.
        #[cfg_attr(docsrs, doc(cfg(feature = "encrypted_bincode")))]
        #[derive(Educe)]
        #[educe(Debug)]
        pub struct EncryptedBincode<Item, SinkItem, O = bincode_crate::DefaultOptions> {
            #[educe(Debug(ignore))]
            options: O,
            #[educe(Debug(ignore))]
            ghost: PhantomData<(Item, SinkItem)>,
            #[educe(Debug(ignore))]
            key: Secret<Vec<u8>>,
        }

        impl<Item, SinkItem, O> EncryptedBincode<Item, SinkItem, O>
        where
            O: Options
        {
            pub fn new(key: Vec<u8>, opts: Option<O>) -> Self {
                let key = Secret::new(key);
                    Self {
                        options: opts.unwrap_or(bincode_crate::DefaultOptions::default()),
                        ghost: PhantomData,
                        key
                    }
            }
        }

        impl<Item, SinkItem> Default for EncryptedBincode<Item, SinkItem> {
            fn default() -> Self {
                EncryptedBincode {
                    options: Default::default(),
                    ghost: PhantomData,
                    key: gen_key(),
                }
            }
        }

        impl<Item, SinkItem, O> From<O> for EncryptedBincode<Item, SinkItem, O>
        where
            O: Options,
        {
            fn from(options: O) -> Self {
                Self {
                    options,
                    ghost: PhantomData,
                    key: gen_key(),
                }
            }
        }

        #[cfg_attr(docsrs, doc(cfg(feature = "encrypted_bincode")))]
        pub type SymmetricalEncryptedBincode<T, O = bincode_crate::DefaultOptions> =
            EncryptedBincode<T, T, O>;

        impl<Item, SinkItem, O> Deserializer<Item> for EncryptedBincode<Item, SinkItem, O>
        where
            for<'a> Item: Deserialize<'a>,
            O: Options + Clone,
        {
            type Error = io::Error;

            fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
                let nonce = XNonce::from_slice(&src[..24]);
                let chacha: XChaCha20Poly1305 = XChaCha20Poly1305::new(Key::from_slice(self.key.expose_secret()));
                let data = chacha
                    .decrypt(nonce, &src[24..])
                    .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                self.options
                    .clone()
                    .deserialize(&data)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
        }

        impl<Item, SinkItem, O> Serializer<SinkItem> for EncryptedBincode<Item, SinkItem, O>
        where
            SinkItem: Serialize,
            O: Options + Clone,
        {
            type Error = io::Error;

            fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
                let mut nonce = XNonce::default();
                let mut rng = OsRng::default();
                rng.fill_bytes(&mut nonce);
                let key = Key::from_slice(self.key.expose_secret());
                let cipher = XChaCha20Poly1305::new(key);
                let mut res = nonce.to_vec();
                let ser = self
                    .options
                    .clone()
                    .serialize(&item)
                    .map_err(|e| io::Error::new(ErrorKind::InvalidInput, e))?;
                let mut other = cipher
                    .encrypt(&nonce, ser.as_slice())
                    .map_err(|e| io::Error::new(ErrorKind::InvalidData, e))?;
                res.append(&mut other);
                Ok(Bytes::from(res))
            }
        }
    }
    #[cfg(feature = "bincode")]
    mod bincode {
        use super::*;
        use bincode_crate::config::Options;
        use serde::{Deserialize, Serialize};
        use std::io;

        /// Bincode codec using [bincode](https://docs.rs/bincode) crate.
        #[cfg_attr(docsrs, doc(cfg(feature = "bincode")))]
        #[derive(Educe)]
        #[educe(Debug)]
        pub struct Bincode<Item, SinkItem, O = bincode_crate::DefaultOptions> {
            #[educe(Debug(ignore))]
            options: O,
            #[educe(Debug(ignore))]
            ghost: PhantomData<(Item, SinkItem)>,
        }

        impl<Item, SinkItem> Default for Bincode<Item, SinkItem> {
            fn default() -> Self {
                Bincode {
                    options: Default::default(),
                    ghost: PhantomData,
                }
            }
        }

        impl<Item, SinkItem, O> From<O> for Bincode<Item, SinkItem, O>
        where
            O: Options,
        {
            fn from(options: O) -> Self {
                Self {
                    options,
                    ghost: PhantomData,
                }
            }
        }

        #[cfg_attr(docsrs, doc(cfg(feature = "bincode")))]
        pub type SymmetricalBincode<T, O = bincode_crate::DefaultOptions> = Bincode<T, T, O>;

        impl<Item, SinkItem, O> Deserializer<Item> for Bincode<Item, SinkItem, O>
        where
            for<'a> Item: Deserialize<'a>,
            O: Options + Clone,
        {
            type Error = io::Error;

            fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
                self.options
                    .clone()
                    .deserialize(src)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
        }

        impl<Item, SinkItem, O> Serializer<SinkItem> for Bincode<Item, SinkItem, O>
        where
            SinkItem: Serialize,
            O: Options + Clone,
        {
            type Error = io::Error;

            fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
                Ok(self
                    .options
                    .clone()
                    .serialize(item)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                    .into())
            }
        }
    }

    #[cfg(feature = "json")]
    mod json {
        use super::*;
        use bytes::Buf;
        use serde::{Deserialize, Serialize};

        /// JSON codec using [serde_json](https://docs.rs/serde_json) crate.
        #[cfg_attr(docsrs, doc(cfg(feature = "json")))]
        #[derive(Educe)]
        #[educe(Debug, Default)]
        pub struct Json<Item, SinkItem> {
            #[educe(Debug(ignore), Default(expression = "PhantomData"))]
            ghost: PhantomData<(Item, SinkItem)>,
        }

        #[cfg_attr(docsrs, doc(cfg(feature = "json")))]
        pub type SymmetricalJson<T> = Json<T, T>;

        impl<Item, SinkItem> Deserializer<Item> for Json<Item, SinkItem>
        where
            for<'a> Item: Deserialize<'a>,
        {
            type Error = serde_json::Error;

            fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
                serde_json::from_reader(std::io::Cursor::new(src).reader())
            }
        }

        impl<Item, SinkItem> Serializer<SinkItem> for Json<Item, SinkItem>
        where
            SinkItem: Serialize,
        {
            type Error = serde_json::Error;

            fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
                serde_json::to_vec(item).map(Into::into)
            }
        }
    }

    #[cfg(feature = "messagepack")]
    mod messagepack {
        use super::*;
        use bytes::Buf;
        use serde::{Deserialize, Serialize};
        use std::io;

        /// MessagePack codec using [rmp-serde](https://docs.rs/rmp-serde) crate.
        #[cfg_attr(docsrs, doc(cfg(feature = "messagepack")))]
        #[derive(Educe)]
        #[educe(Debug, Default)]
        pub struct MessagePack<Item, SinkItem> {
            #[educe(Debug(ignore), Default(expression = "PhantomData"))]
            ghost: PhantomData<(Item, SinkItem)>,
        }

        #[cfg_attr(docsrs, doc(cfg(feature = "messagepack")))]
        pub type SymmetricalMessagePack<T> = MessagePack<T, T>;

        impl<Item, SinkItem> Deserializer<Item> for MessagePack<Item, SinkItem>
        where
            for<'a> Item: Deserialize<'a>,
        {
            type Error = io::Error;

            fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
                rmp_serde::from_read(std::io::Cursor::new(src).reader())
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
            }
        }

        impl<Item, SinkItem> Serializer<SinkItem> for MessagePack<Item, SinkItem>
        where
            SinkItem: Serialize,
        {
            type Error = io::Error;

            fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
                Ok(rmp_serde::to_vec(item)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
                    .into())
            }
        }
    }

    #[cfg(feature = "cbor")]
    mod cbor {
        use super::*;
        use serde::{Deserialize, Serialize};
        use std::io;

        /// CBOR codec using [serde_cbor](https://docs.rs/serde_cbor) crate.
        #[cfg_attr(docsrs, doc(cfg(feature = "cbor")))]
        #[derive(Educe)]
        #[educe(Debug, Default)]
        pub struct Cbor<Item, SinkItem> {
            #[educe(Debug(ignore), Default(expression = "PhantomData"))]
            _mkr: PhantomData<(Item, SinkItem)>,
        }

        #[cfg_attr(docsrs, doc(cfg(feature = "cbor")))]
        pub type SymmetricalCbor<T> = Cbor<T, T>;

        impl<Item, SinkItem> Deserializer<Item> for Cbor<Item, SinkItem>
        where
            for<'a> Item: Deserialize<'a>,
        {
            type Error = io::Error;

            fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
                serde_cbor::from_slice(src.as_ref()).map_err(into_io_error)
            }
        }

        impl<Item, SinkItem> Serializer<SinkItem> for Cbor<Item, SinkItem>
        where
            SinkItem: Serialize,
        {
            type Error = io::Error;

            fn serialize(self: Pin<&mut Self>, item: &SinkItem) -> Result<Bytes, Self::Error> {
                serde_cbor::to_vec(item)
                    .map_err(into_io_error)
                    .map(Into::into)
            }
        }

        fn into_io_error(cbor_err: serde_cbor::Error) -> io::Error {
            use io::ErrorKind;
            use serde_cbor::error::Category;
            use std::error::Error;

            match cbor_err.classify() {
                Category::Eof => io::Error::new(ErrorKind::UnexpectedEof, cbor_err),
                Category::Syntax => io::Error::new(ErrorKind::InvalidInput, cbor_err),
                Category::Data => io::Error::new(ErrorKind::InvalidData, cbor_err),
                Category::Io => {
                    // Extract the underlying io error's type
                    let kind = cbor_err
                        .source()
                        .and_then(|err| err.downcast_ref::<io::Error>())
                        .map(|io_err| io_err.kind())
                        .unwrap_or(ErrorKind::Other);
                    io::Error::new(kind, cbor_err)
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "encrypted_bincode")]
    #[test]
    fn enc_bincode_impls() {
        use impls::impls;
        use std::fmt::Debug;

        struct Nothing;
        type T = crate::formats::EncryptedBincode<Nothing, Nothing>;

        assert!(impls!(T: Debug));
        assert!(impls!(T: Default));
    }
    #[cfg(feature = "bincode")]
    #[test]
    fn bincode_impls() {
        use impls::impls;
        use std::fmt::Debug;

        struct Nothing;
        type T = crate::formats::Bincode<Nothing, Nothing>;

        assert!(impls!(T: Debug));
        assert!(impls!(T: Default));
    }

    #[cfg(feature = "json")]
    #[test]
    fn json_impls() {
        use impls::impls;
        use std::fmt::Debug;

        struct Nothing;
        type T = crate::formats::Json<Nothing, Nothing>;

        assert!(impls!(T: Debug));
        assert!(impls!(T: Default));
    }

    #[cfg(feature = "messagepack")]
    #[test]
    fn messagepack_impls() {
        use impls::impls;
        use std::fmt::Debug;

        struct Nothing;
        type T = crate::formats::MessagePack<Nothing, Nothing>;

        assert!(impls!(T: Debug));
        assert!(impls!(T: Default));
    }

    #[cfg(feature = "cbor")]
    #[test]
    fn cbor_impls() {
        use impls::impls;
        use std::fmt::Debug;

        struct Nothing;
        type T = crate::formats::Cbor<Nothing, Nothing>;

        assert!(impls!(T: Debug));
        assert!(impls!(T: Default));
    }
}
