// Copyright (c) Facebook, Inc. and its affiliates.
//
// This source code is licensed under the MIT license found in the
// LICENSE file in the "hack" directory of this source tree.
#![allow(unused_variables)]
#![allow(dead_code)]

use std::{
    fmt::{self, Debug},
    io,
};
use thiserror::Error;

pub type Result<T, WE> = std::result::Result<T, Error<WE>>;

#[macro_export]
macro_rules! not_impl {
    () => {
        Err(Error::NotImpl(format!("{}:{}", file!(), line!())))
    };
}

#[derive(Error, Debug)]
pub enum Error<WE: Debug> {
    #[error("write error: {0:?}")]
    WriteError(WE),

    #[error("a string may contain invalid utf-8")]
    InvalidUTF8,

    //TODO(hrust): This is a temp error during porting
    #[error("NOT_IMPL: {0}")]
    NotImpl(String),

    #[error("Failed: {0}")]
    Fail(String),
}

pub trait Write {
    type Error: Debug;
    fn write(&mut self, s: impl AsRef<str>) -> Result<(), Self::Error>;
}

impl<W: fmt::Write> Write for W {
    type Error = fmt::Error;
    fn write(&mut self, s: impl AsRef<str>) -> Result<(), Self::Error> {
        self.write_str(s.as_ref()).map_err(Error::WriteError)
    }
}

pub struct IoWrite(Box<dyn io::Write + Send>);

impl IoWrite {
    pub fn new(w: impl io::Write + 'static + Send) -> Self {
        Self(Box::new(w))
    }

    pub fn flush(&mut self) -> std::result::Result<(), io::Error> {
        self.0.flush()
    }
}

impl Write for IoWrite {
    type Error = std::io::Error;
    fn write(&mut self, s: impl AsRef<str>) -> Result<(), Self::Error> {
        self.0
            .write_all(s.as_ref().as_bytes())
            .map_err(Error::WriteError)
    }
}

pub fn newline<W: Write>(w: &mut W) -> Result<(), W::Error> {
    w.write("\n")
}

pub fn wrap_by_<W, F>(w: &mut W, s: &str, e: &str, f: F) -> Result<(), W::Error>
where
    W: Write,
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    w.write(s)?;
    f(w)?;
    w.write(e)
}

pub fn wrap_by<W: Write, F>(w: &mut W, s: &str, f: F) -> Result<(), W::Error>
where
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    wrap_by_(w, s, s, f)
}

pub fn wrap_by_braces<W: Write, F>(w: &mut W, f: F) -> Result<(), W::Error>
where
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    wrap_by_(w, "{", "}", f)
}

pub fn wrap_by_paren<W: Write, F>(w: &mut W, f: F) -> Result<(), W::Error>
where
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    wrap_by_(w, "(", ")", f)
}

pub fn wrap_by_quotes<W: Write, F>(w: &mut W, f: F) -> Result<(), W::Error>
where
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    wrap_by_(w, "\"", "\"", f)
}

pub fn wrap_by_angle<W: Write, F>(w: &mut W, f: F) -> Result<(), W::Error>
where
    F: FnOnce(&mut W) -> Result<(), W::Error>,
{
    wrap_by_(w, "<", ">", f)
}

pub fn write_list<W: Write>(w: &mut W, items: &[impl AsRef<str>]) -> Result<(), W::Error> {
    Ok(for i in items {
        w.write(i.as_ref())?;
    })
}

pub fn concat_str<W: Write, I: AsRef<str>>(w: &mut W, ss: impl AsRef<[I]>) -> Result<(), W::Error> {
    concat(w, ss, |w, s| w.write(s))
}

pub fn concat_str_by<W: Write, I: AsRef<str>>(
    w: &mut W,
    sep: impl AsRef<str>,
    ss: impl AsRef<[I]>,
) -> Result<(), W::Error> {
    concat_by(w, sep, ss, |w, s| w.write(s))
}

pub fn concat<W, T, F>(w: &mut W, items: impl AsRef<[T]>, f: F) -> Result<(), W::Error>
where
    W: Write,
    F: FnMut(&mut W, &T) -> Result<(), W::Error>,
{
    concat_by(w, "", items, f)
}

pub fn concat_by<W, T, F>(
    w: &mut W,
    sep: impl AsRef<str>,
    items: impl AsRef<[T]>,
    mut f: F,
) -> Result<(), W::Error>
where
    W: Write,
    F: FnMut(&mut W, &T) -> Result<(), W::Error>,
{
    let mut first = true;
    let sep = sep.as_ref();
    Ok(for i in items.as_ref() {
        if first {
            first = false;
        } else {
            w.write(sep)?;
        }
        f(w, i)?;
    })
}

pub fn option<W: Write, T, F>(w: &mut W, i: impl Into<Option<T>>, f: F) -> Result<(), W::Error>
where
    F: Fn(&mut W, T) -> Result<(), W::Error>,
{
    match i.into() {
        None => Ok(()),
        Some(i) => f(w, i),
    }
}

pub fn option_or<W: Write, T, F>(
    w: &mut W,
    i: Option<T>,
    f: F,
    default: impl AsRef<str>,
) -> Result<(), W::Error>
where
    F: Fn(&mut W, T) -> Result<(), W::Error>,
{
    match i {
        None => w.write(default),
        Some(i) => f(w, i),
    }
}
