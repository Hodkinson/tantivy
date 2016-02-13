use std::io::Write;
use std::io::BufWriter;
use std::io::Read;
use std::io::Cursor;
use std::io::SeekFrom;
use std::io::Seek;
use std::marker::PhantomData;
use core::DocId;
use std::ops::DerefMut;
use bincode;
use byteorder;
use core::error;
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use std::fmt;


pub trait BinarySerializable : fmt::Debug + Sized {
    // TODO move Result from Error.
    fn serialize(&self, writer: &mut Write) -> error::Result<usize>;
    fn deserialize(reader: &mut Read) -> error::Result<Self>;
}

impl BinarySerializable for () {
    fn serialize(&self, writer: &mut Write) -> error::Result<usize> {
        Ok(0)
    }
    fn deserialize(reader: &mut Read) -> error::Result<Self> {
        Ok(())
    }
}

impl<T: BinarySerializable> BinarySerializable for Vec<T> {
    fn serialize(&self, writer: &mut Write) -> error::Result<usize> {
        let mut total_size = 0;
        writer.write_u32::<BigEndian>(self.len() as u32);
        total_size += 4;
        for it in self.iter() {
            let item_size = try!(it.serialize(writer));
            total_size += item_size;
        }
        Ok(total_size)
    }
    fn deserialize(reader: &mut Read) -> error::Result<Vec<T>> {
        // TODO error
        let num_items = reader.read_u32::<BigEndian>().unwrap();
        let mut items: Vec<T> = Vec::with_capacity(num_items as usize);
        for i in 0..num_items {
            let item = try!(T::deserialize(reader));
            items.push(item);
        }
        Ok(items)
    }
}

struct LayerBuilder {
    period: usize,
    buffer: Vec<u8>,
    remaining: usize,
    len: usize,
}

impl LayerBuilder {

    fn written_size(&self,) -> usize {
        self.buffer.len()
    }

    fn write(&self, output: &mut Write) -> Result<(), byteorder::Error> {
        try!(output.write_all(&self.buffer));
        Ok(())
    }

    fn len(&self,) -> usize {
        self.len
    }

    fn with_period(period: usize) -> LayerBuilder {
        LayerBuilder {
            period: period,
            buffer: Vec::new(),
            remaining: period,
            len: 0,
        }
    }

    fn insert<S: BinarySerializable>(&mut self, doc_id: DocId, value: &S) -> Option<(DocId, u32)> {
        self.remaining -= 1;
        self.len += 1;
        let offset = self.written_size() as u32; // TODO not sure if we want after or here
        let mut res;
        if self.remaining == 0 {
            self.remaining = self.period;
            res = Some((doc_id, offset));
        }
        else {
            res = None;
        }
        self.buffer.write_u32::<BigEndian>(doc_id);
        value.serialize(&mut self.buffer);
        res
    }
}


pub struct SkipListBuilder {
    period: usize,
    layers: Vec<LayerBuilder>,
}


impl SkipListBuilder {

    pub fn new(period: usize) -> SkipListBuilder {
        SkipListBuilder {
            period: period,
            layers: Vec::new(),
        }
    }


    fn get_layer<'a>(&'a mut self, layer_id: usize) -> &mut LayerBuilder {
        if layer_id == self.layers.len() {
            let layer_builder = LayerBuilder::with_period(self.period);
            self.layers.push(layer_builder);
        }
        &mut self.layers[layer_id]
    }

    pub fn insert<S: BinarySerializable>(&mut self, doc_id: DocId, dest: &S) {
        let mut layer_id = 0;
        let mut skip_pointer = self.get_layer(layer_id).insert(doc_id, dest);
        loop {
            layer_id += 1;
            println!("skip pointer {:?}", skip_pointer);
            skip_pointer = match skip_pointer {
                Some((skip_doc_id, skip_offset)) =>
                    self
                        .get_layer(layer_id)
                        .insert(skip_doc_id, &skip_offset),
                None => { return; }
            };
        }
    }

    pub fn write<W: Write>(self, output: &mut Write) -> error::Result<()> {
        let mut size: u32 = 0;
        let mut layer_sizes: Vec<u32> = Vec::new();
        for layer in self.layers.iter() {
            size += layer.buffer.len() as u32;
            layer_sizes.push(size);
        }
        layer_sizes.serialize(output);
        for layer in self.layers.iter() {
            match layer.write(output) {
                Ok(())=> {},
                Err(someerr)=> { return Err(error::Error::WriteError(format!("Could not write skiplist {:?}", someerr) )) }
            }
        }
        Ok(())
    }
}


impl BinarySerializable for u32 {
    fn serialize(&self, writer: &mut Write) -> error::Result<usize> {
        // TODO error handling
        writer.write_u32::<BigEndian>(self.clone());
        Ok(4)
    }

    fn deserialize(reader: &mut Read) -> error::Result<Self> {
        // TODO error handling
        reader.read_u32::<BigEndian>().map_err(|err| error::Error::ReadError)
    }
}



struct Layer<'a, T> {
    cursor: Cursor<&'a [u8]>,
    next_id: DocId,
    _phantom_: PhantomData<T>,
}


impl<'a, T: BinarySerializable> Iterator for Layer<'a, T> {

    type Item = (DocId, T);

    fn next(&mut self,)-> Option<(DocId, T)> {
        println!("eeeeee");
        if self.next_id == u32::max_value() {
            None
        }
        else {
            let cur_val = T::deserialize(&mut self.cursor).unwrap();
            let cur_id = self.next_id;
            self.next_id =
                match u32::deserialize(&mut self.cursor) {
                    Ok(val) => val,
                    Err(_) => u32::max_value()
                };
            Some((cur_id, cur_val))
        }
    }
}


static EMPTY: [u8; 0] = [];

impl<'a, T: BinarySerializable> Layer<'a, T> {

    fn read(mut cursor: Cursor<&'a [u8]>) -> Layer<'a, T> {
        // TODO error handling?
        let next_id = match cursor.read_u32::<BigEndian>() {
            Ok(val) => val,
            Err(_) => u32::max_value(),
        };
        Layer {
            cursor: cursor,
            next_id: next_id,
            _phantom_: PhantomData,
        }
    }

    fn empty() -> Layer<'a, T> {
        Layer {
            cursor: Cursor::new(&EMPTY),
            next_id: DocId::max_value(),
            _phantom_: PhantomData,
        }
    }


    fn seek_offset(&mut self, offset: usize) {
        self.cursor.seek(SeekFrom::Start(offset as u64));
        self.next_id = match self.cursor.read_u32::<BigEndian>() {
            Ok(val) => val,
            Err(_) => u32::max_value(),
        };
    }

    // Returns the last element (key, val)
    // such that (key < doc_id)
    //
    // If there is no such element anymore,
    // returns None.
    fn seek(&mut self, doc_id: DocId) -> Option<(DocId, T)> {
        let mut val = None;
        while self.next_id < doc_id {
            match self.next() {
                None => { break; },
                v => { val = v; }
            }
        }
        val
    }
}

pub struct SkipList<'a, T: BinarySerializable> {
    data_layer: Layer<'a, T>,
    skip_layers: Vec<Layer<'a, u32>>,
}

impl<'a, T: BinarySerializable> Iterator for SkipList<'a, T> {

    type Item = (DocId, T);

    fn next(&mut self,)-> Option<(DocId, T)> {
        self.data_layer.next()
    }
}

impl<'a, T: BinarySerializable> SkipList<'a, T> {

    pub fn seek(&mut self, doc_id: DocId) -> Option<(DocId, T)> {
        let mut next_layer_skip: Option<(DocId, u32)> = None;
        for skip_layer_id in 0..self.skip_layers.len() {
            let mut skip_layer: &mut Layer<'a, u32> = &mut self.skip_layers[skip_layer_id];
            println!("\n\nLAYER {}", skip_layer_id);
            println!("nextid before skip {}", skip_layer.next_id);
            match next_layer_skip {
                 Some((_, offset)) => { skip_layer.seek_offset(offset as usize); },
                 None => {}
             };
             println!("nextid after skip {}", skip_layer.next_id);
             next_layer_skip = skip_layer.seek(doc_id);
             println!("nextid after seek {}", skip_layer.next_id);
             println!("--- nextlayerskip {:?}", next_layer_skip);
         }
         match next_layer_skip {
             Some((_, offset)) => { self.data_layer.seek_offset(offset as usize); },
             None => {}
         };
         self.data_layer.seek(doc_id)
    }

    pub fn read(data: &'a [u8]) -> SkipList<'a, T> {
        let mut cursor = Cursor::new(data);
        let offsets: Vec<u32> = Vec::deserialize(&mut cursor).unwrap();
        println!("offsets {:?}", offsets);
        let num_layers = offsets.len();
        println!("{} layers ", num_layers);

        let start_position = cursor.position() as usize;
        let layers_data: &[u8] = &data[start_position..data.len()];

        let data_layer: Layer<'a, T> =
            if num_layers == 0 { Layer::empty() }
            else {
                let first_layer_data: &[u8] = &layers_data[..offsets[0] as usize];
                let first_layer_cursor = Cursor::new(first_layer_data);
                Layer::read(first_layer_cursor)
            };
        let mut skip_layers: Vec<Layer<u32>>;
        if num_layers > 0 {
            skip_layers = offsets.iter()
                .zip(&offsets[1..])
                .map(|(start, stop)| {
                    let layer_data: &[u8] = &layers_data[*start as usize..*stop as usize];
                    let cursor = Cursor::new(layer_data);
                    Layer::read(cursor)
                })
                .collect();
        }
        else {
            skip_layers = Vec::new();
        }
        skip_layers.reverse();
        SkipList {
            skip_layers: skip_layers,
            data_layer: data_layer,
        }
    }
}
