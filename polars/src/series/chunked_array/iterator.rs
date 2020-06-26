use crate::datatypes::Utf8Chunked;
use crate::series::chunked_array::builder::{PrimitiveChunkedBuilder, Utf8ChunkedBuilder};
use crate::series::chunked_array::Downcast;
use crate::{
    datatypes,
    series::chunked_array::{ChunkedArray, SeriesOps},
};
use arrow::array::{Array, ArrayDataRef, PrimitiveArray, PrimitiveArrayOps, StringArray};
use arrow::datatypes::{ArrowNumericType, ArrowPrimitiveType};
use std::iter::FromIterator;
use std::marker::PhantomData;
use std::slice::Iter;
use unsafe_unwrap::UnsafeUnwrap;

/// TODO: Split the ChunkIterState in separate structs.

// This module implements an iter method for both ArrowPrimitiveType and Utf8 type.
// As both expose a different api in some, this required some hacking.
// A solution was found by returning an auxiliary struct ChunkIterState from the .iter methods.
// This ChunkIterState has a generic type T: ArrowPrimitiveType to work well with the some api.
// It also has a phantomtype K that is only used as distinctive type to be able to implement a trait
// method twice. Sort of a specialization hack.

// K is a phantom type to make specialization work.
// K is set to u8 for all ArrowPrimitiveTypes
// K is set to String for datatypes::Utf8DataType
pub struct ChunkIterState<'a, T, K>
where
    T: ArrowPrimitiveType,
{
    array_primitive: Option<Vec<&'a PrimitiveArray<T>>>,
    array_string: Option<Vec<&'a StringArray>>,
    current_data: ArrayDataRef,
    current_primitive: Option<&'a PrimitiveArray<T>>,
    current_string: Option<&'a StringArray>,
    chunk_i: usize,
    array_i: usize,
    length: usize,
    n_chunks: usize,
    phantom: PhantomData<K>,
}

impl<T, S> ChunkIterState<'_, T, S>
where
    T: ArrowPrimitiveType,
{
    #[inline]
    fn set_indexes(&mut self, arr: &dyn Array) {
        self.array_i += 1;
        if self.array_i >= arr.len() {
            // go to next array in the chunks
            self.array_i = 0;
            self.chunk_i += 1;

            if let Some(arrays) = &self.array_primitive {
                if self.chunk_i < arrays.len() {
                    // not yet at last chunk
                    let arr = unsafe { *arrays.get_unchecked(self.chunk_i) };
                    self.current_data = arr.data();
                    self.current_primitive = Some(arr);
                }
            }
            if let Some(arrays) = &self.array_string {
                if self.chunk_i < arrays.len() {
                    // not yet at last chunk
                    let arr = unsafe { *arrays.get_unchecked(self.chunk_i) };
                    self.current_data = arr.data();
                    self.current_string = Some(arr);
                }
            }
        }
    }

    #[inline]
    fn out_of_bounds(&self) -> bool {
        self.chunk_i >= self.n_chunks
    }
}

impl<T> Iterator for ChunkIterState<'_, T, u8>
where
    T: ArrowPrimitiveType,
{
    // nullable, therefore an option
    type Item = Option<T::Native>;

    /// Because some types are nullable an option is returned. This is wrapped in another option
    /// to indicate if the iterator returns Some or None.
    fn next(&mut self) -> Option<Self::Item> {
        if self.out_of_bounds() {
            return None;
        }

        debug_assert!(self.array_primitive.is_some());
        let arrays = unsafe { self.array_primitive.as_ref().take().unsafe_unwrap() };

        debug_assert!(self.chunk_i < arrays.len());
        let ret;
        let arr = unsafe { self.current_primitive.unsafe_unwrap() };

        if self.current_data.is_null(self.array_i) {
            ret = Some(None)
        } else {
            let v = arr.value(self.array_i);
            ret = Some(Some(v));
        }
        self.set_indexes(arr);
        ret
    }
}

impl<'a, T> Iterator for ChunkIterState<'a, T, String>
where
    T: ArrowPrimitiveType,
{
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        if self.out_of_bounds() {
            return None;
        }

        debug_assert!(self.array_string.is_some());
        let arr = unsafe { self.current_string.unsafe_unwrap() };
        let v = arr.value(self.array_i);
        self.set_indexes(arr);
        Some(v)
    }
}

pub trait ChunkIterator<T, S>
where
    T: ArrowPrimitiveType,
{
    fn iter(&self) -> ChunkIterState<T, S>;
}

/// ChunkIter for all the ArrowPrimitiveTypes
/// Note that u8 is only a phantom type to be able to have stable specialization.
impl<T> ChunkIterator<T, u8> for ChunkedArray<T>
where
    T: ArrowPrimitiveType,
{
    fn iter(&self) -> ChunkIterState<T, u8> {
        let arrays = self.downcast_chunks();
        let arr = unsafe { *arrays.get_unchecked(0) };
        let data = arr.data();

        ChunkIterState {
            array_primitive: Some(arrays),
            array_string: None,
            current_data: data,
            current_primitive: Some(arr),
            current_string: None,
            chunk_i: 0,
            array_i: 0,
            length: self.len(),
            n_chunks: self.chunks.len(),
            phantom: PhantomData,
        }
    }
}

/// ChunkIter for the Utf8Type
/// Note that datatypes::Int32Type is just a random ArrowPrimitveType for the Left variant
/// of the Either enum. We don't need it.
impl ChunkIterator<datatypes::Int32Type, String> for ChunkedArray<datatypes::Utf8Type> {
    fn iter(&self) -> ChunkIterState<datatypes::Int32Type, String> {
        let arrays = self.downcast_chunks();
        let arr = unsafe { *arrays.get_unchecked(0) };
        let data = arr.data();

        ChunkIterState {
            array_primitive: None,
            array_string: Some(arrays),
            current_data: data,
            current_primitive: None,
            current_string: Some(arr),
            chunk_i: 0,
            array_i: 0,
            length: self.len(),
            n_chunks: self.chunks.len(),
            phantom: PhantomData,
        }
    }
}

/// Specialized Iterator for ChunkedArray<PolarsNumericType>
pub struct ChunkNumIter<'a, T>
where
    T: ArrowNumericType,
{
    array_chunks: Vec<&'a PrimitiveArray<T>>,
    current_data: Option<ArrayDataRef>,
    current_array: Option<&'a PrimitiveArray<T>>,
    current_len: usize,
    chunk_i: usize,
    array_i: usize,
    length: usize,
    n_chunks: usize,
    opt_slice: Option<Iter<'a, T::Native>>,
}

impl<'a, T> Iterator for ChunkNumIter<'a, T>
where
    T: ArrowNumericType,
{
    type Item = Option<T::Native>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(iter) = &mut self.opt_slice {
            return iter.next().map(|&v| Some(v));
        }

        if self.out_of_bounds() {
            return None;
        }

        let current_data = unsafe { self.current_data.as_ref().unsafe_unwrap() };
        let current_array = unsafe { self.current_array.unsafe_unwrap() };

        debug_assert!(self.chunk_i < self.array_chunks.len());
        let ret;

        if current_data.is_null(self.array_i) {
            ret = Some(None)
        } else {
            let v = current_array.value(self.array_i);
            ret = Some(Some(v));
        }
        self.set_indexes();
        ret
    }
}

impl<'a, T> ChunkNumIter<'a, T>
where
    T: ArrowNumericType,
{
    #[inline]
    fn set_indexes(&mut self) {
        self.array_i += 1;
        if self.array_i >= self.current_len {
            // go to next array in the chunks
            self.array_i = 0;
            self.chunk_i += 1;

            if self.chunk_i < self.array_chunks.len() {
                // not yet at last chunk
                let arr = unsafe { *self.array_chunks.get_unchecked(self.chunk_i) };
                self.current_data = Some(arr.data());
                self.current_array = Some(arr);
            }
        }
    }

    #[inline]
    fn out_of_bounds(&self) -> bool {
        self.chunk_i >= self.n_chunks
    }
}

impl<'a, T> IntoIterator for &'a ChunkedArray<T>
where
    T: ArrowNumericType,
{
    type Item = Option<T::Native>;
    type IntoIter = ChunkNumIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        let arrays = self.downcast_chunks();

        let arr = arrays.get(0).map(|v| *v);
        let data = arr.map(|arr| arr.data());

        let opt_slice = match self.cont_slice() {
            Ok(slice) => Some(slice.iter()),
            Err(_) => None,
        };
        let current_len = match arr {
            Some(arr) => arr.len(),
            None => 0,
        };

        ChunkNumIter {
            array_chunks: arrays,
            current_data: data,
            current_array: arr,
            current_len,
            chunk_i: 0,
            array_i: 0,
            length: self.len(),
            n_chunks: self.chunks.len(),
            opt_slice,
        }
    }
}

impl<T> FromIterator<Option<T::Native>> for ChunkedArray<T>
where
    T: ArrowPrimitiveType,
{
    fn from_iter<I: IntoIterator<Item = Option<T::Native>>>(iter: I) -> Self {
        let mut builder = PrimitiveChunkedBuilder::new("", 1024);

        for opt_val in iter {
            builder.append_option(opt_val).expect("could not append");
        }

        builder.finish()
    }
}

impl<'a> FromIterator<&'a str> for Utf8Chunked {
    fn from_iter<I: IntoIterator<Item = &'a str>>(iter: I) -> Self {
        let mut builder = Utf8ChunkedBuilder::new("", 1024);

        for val in iter {
            builder.append_value(val).expect("could not append");
        }
        builder.finish()
    }
}

#[cfg(test)]
mod test {
    use crate::prelude::*;

    #[test]
    fn out_of_bounds() {
        let a = UInt32Chunked::new_from_slice("a", &[1, 2, 3]);
        let v = a.iter().collect::<Vec<_>>();
    }
}
