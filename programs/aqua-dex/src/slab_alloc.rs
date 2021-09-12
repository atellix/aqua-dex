use bytemuck::{ from_bytes, from_bytes_mut, cast, cast_mut, cast_ref, cast_slice, cast_slice_mut, Pod, Zeroable };
use num_enum::{ IntoPrimitive, TryFromPrimitive };
use arrayref::{ array_refs, mut_array_refs };
use static_assertions::const_assert_eq;
use solana_program::msg;
use murmur3::murmur3_x86_128;
use std::{ 
//    fmt,
    io::Cursor,
    convert::{ identity, TryFrom },
    mem::{ align_of, size_of }
};

// Data node

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum DataNodeTag {
    Uninitialized = 0,
    DataNode = 1,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(packed)]
pub struct DataNode {
    tag: u8,
    data: [u64; 2],
}
unsafe impl Zeroable for DataNode {}
unsafe impl Pod for DataNode {}

impl DataNode {
    #[inline]
    pub fn new(
        data: [u64; 2],
    ) -> Self {
        DataNode {
            tag: DataNodeTag::DataNode.into(),
            data,
        }
    }

    #[inline]
    pub fn data(&self) -> [u64; 2] {
        self.data
    }
}

// Slab page allocator

const PAGE_SIZE: usize = 16384; // bytes (16K)
const PAGE_MAX: usize = 8; // 0..638 for 10MiB @ 16K / page
const TYPE_MAX_PAGES: usize = 4; // Up to PAGE_MAX
const TYPE_MAX: usize = 4;

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct TypePages {
    header_size: usize,
    offset_size: usize,
    alloc_items: usize,
    type_pages: [u16; TYPE_MAX_PAGES],
}
unsafe impl Zeroable for TypePages {}
unsafe impl Pod for TypePages {}

impl TypePages {
    #[inline]
    pub fn new() -> Self {
        TypePages {
            header_size: 0,
            offset_size: 0,
            alloc_items: 0, // Total items
            type_pages: [0; TYPE_MAX_PAGES],
        }
    }

    #[inline]
    pub fn alloc_items(&self) -> usize {
        self.alloc_items
    }

    #[inline]
    pub fn set_alloc_items(&mut self, alloc_items: usize) {
        self.alloc_items = alloc_items
    }

    #[inline]
    pub fn header_size(&self) -> usize {
        self.header_size
    }

    #[inline]
    pub fn set_offset_size(&mut self, offset_size: usize) {
        self.offset_size = offset_size
    }

    #[inline]
    pub fn offset_size(&self) -> usize {
        self.offset_size
    }

    #[inline]
    pub fn set_header_size(&mut self, header_size: usize) {
        self.header_size = header_size
    }

    #[inline]
    pub fn get_page(self, idx: usize) -> u16 {
        self.type_pages[idx]
    }

    #[inline]
    pub fn set_page(&mut self, idx: usize, page: u16) {
        self.type_pages[idx] = page
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct PageData {
    data: [u8; PAGE_SIZE],
}
unsafe impl Zeroable for PageData {}
unsafe impl Pod for PageData {}

impl PageData {
    #[inline]
    pub fn data<T: bytemuck::Pod>(&self, header_size: usize, offset_size: usize) -> &[u8] {
        let combined_size = header_size + offset_size;
        let len_without_header = PAGE_SIZE.checked_sub(combined_size).unwrap();
        let slop = len_without_header % size_of::<T>();
        let truncated_len = PAGE_SIZE - slop;
        &self.data[combined_size..truncated_len]
    }

    #[inline]
    pub fn data_ptr(&self) -> usize {
        self.data[..].as_ptr() as usize
    }

    #[inline]
    pub fn data_mut<T: bytemuck::Pod>(&mut self, header_size: usize, offset_size: usize) -> &mut [u8] {
        let combined_size = header_size + offset_size;
        let len_without_header = PAGE_SIZE.checked_sub(combined_size).unwrap();
        let slop = len_without_header % size_of::<T>();
        let truncated_len = PAGE_SIZE - slop;
        &mut self.data[combined_size..truncated_len]
    }

    #[inline]
    pub fn header<H: bytemuck::Pod>(&self, offset_size: usize) -> &H {
        let header_size = size_of::<H>();
        let bytes = &self.data[offset_size..(header_size + offset_size)];
        /*msg!("from_bytes header");
        msg!("header offset {}", offset_size);
        msg!("header ptr {}", bytes.as_ptr() as usize);
        msg!("header align {}", align_of::<H>());
        msg!("header remain {}", bytes.as_ptr() as usize % align_of::<H>());*/
        from_bytes::<H>(bytes)
    }

    #[inline]
    pub fn header_mut<H: bytemuck::Pod>(&mut self, offset_size: usize) -> &mut H {
        let header_size = size_of::<H>();
        let bytes = &mut self.data[offset_size..(header_size + offset_size)];
        //msg!("from_bytes_mut header_mut");
        from_bytes_mut::<H>(bytes)
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct TypedPageTable {
    top_unused_page: u16,
}
unsafe impl Zeroable for TypedPageTable {}
unsafe impl Pod for TypedPageTable {}

impl TypedPageTable {
    #[inline]
    pub fn new() -> Self {
        TypedPageTable {
            top_unused_page: 0,
            // TODO: total pages in slab
        }
    }
}

const HEADER_SIZE: usize = size_of::<TypedPageTable>();
const TYPES_SIZE: usize = size_of::<[TypePages; TYPE_MAX]>();
const PAGE_TABLE_SIZE: usize = HEADER_SIZE + TYPES_SIZE;

#[derive(Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct SlabPageAlloc([u8]);

impl SlabPageAlloc {
    #[inline]
    pub fn new(bytes: &mut [u8]) -> &mut Self {
        let len_without_table = bytes.len().checked_sub(PAGE_TABLE_SIZE).unwrap();
        let slop = len_without_table % size_of::<PageData>();
        msg!("slab header size: {} - slop: {}", PAGE_TABLE_SIZE, slop);
        let truncated_len = bytes.len() - slop;
        let bytes = &mut bytes[..truncated_len];
        let slab: &mut Self = unsafe { &mut *(bytes as *mut [u8] as *mut SlabPageAlloc) };
        slab.check_size_align();
        slab
    }

    pub fn setup_page_table(&mut self) {
        let (header, _pages) = mut_array_refs![&mut self.0, PAGE_TABLE_SIZE; .. ;];
        let (table, types) = mut_array_refs![header, HEADER_SIZE; .. ;];
        let nt: &mut TypedPageTable = cast_mut(table);
        *nt = TypedPageTable::new();
        let tp: &mut [TypePages] = cast_slice_mut(types);
        //msg!("types: {}", TYPE_MAX);
        for i in 0..TYPE_MAX {
            tp[i] = TypePages::new();
        }
    }

    fn check_size_align(&self) {
        let (header, pages) = array_refs![&self.0, PAGE_TABLE_SIZE; .. ;];
        let (table, types) = array_refs![header, HEADER_SIZE; .. ;];
        let _h: &TypedPageTable = cast_ref(table);
        let _t: &[TypePages] = cast_slice(types);
        let _p: &[PageData] = cast_slice(pages);
    }

    fn parts(&self) -> (&TypedPageTable, &[TypePages], &[PageData]) {
        unsafe {
            invariant(self.0.len() < PAGE_TABLE_SIZE);
            invariant((self.0.as_ptr() as usize) % align_of::<TypedPageTable>() != 0);
            invariant(
                ((self.0.as_ptr() as usize) + PAGE_TABLE_SIZE) % align_of::<PageData>() != 0,
            );
        }

        let (header, pages) = array_refs![&self.0, PAGE_TABLE_SIZE; .. ;];
        let (table, types) = array_refs![header, HEADER_SIZE; .. ;];
        let h: &TypedPageTable = cast_ref(table);
        let t: &[TypePages] = cast_slice(types);
        let p: &[PageData] = cast_slice(pages);
        (h, t, p)
    }

    fn parts_mut(&mut self) -> (&mut TypedPageTable, &mut [TypePages], &mut [PageData]) {
        unsafe {
            invariant(self.0.len() < size_of::<TypedPageTable>());
            invariant((self.0.as_ptr() as usize) % align_of::<PageData>() != 0);
            invariant(
                ((self.0.as_ptr() as usize) + size_of::<TypedPageTable>()) % align_of::<PageData>() != 0,
            );
        }

        let (header, pages) = mut_array_refs![&mut self.0, PAGE_TABLE_SIZE; .. ;];
        let (table, types) = mut_array_refs![header, HEADER_SIZE; .. ;];
        let h: &mut TypedPageTable = cast_mut(table);
        let t: &mut [TypePages] = cast_slice_mut(types);
        let p: &mut [PageData] = cast_slice_mut(pages);
        (h, t, p)
    }

    pub fn allocate<H, T>(&mut self, type_id: u16, items: usize) -> Result<usize, ()> {
        let (page_table, type_table, data_table) = self.parts_mut();
        let item_size: usize = size_of::<T>();
        let header_size: usize = size_of::<H>();

        let next_page = &data_table[page_table.top_unused_page as usize];
        let page_ptr = next_page.data_ptr();
        let header_align = align_of::<H>();
        let offset_size = header_align - (page_ptr % header_align);
        let type_spec = &mut type_table[type_id as usize];
        type_spec.set_offset_size(offset_size);
        type_spec.set_header_size(header_size);

        /*msg!("allocate");
        msg!("allocate offset {}", offset_size);
        msg!("allocate ptr {}", page_ptr as usize);
        msg!("allocate align {}", align_of::<H>());
        msg!("allocate remain {}", page_ptr as usize % align_of::<H>());*/
 
        let items_per_page: usize = (PAGE_SIZE - (offset_size + header_size)) / item_size;
        let pages: &mut usize = &mut 0;
        *pages = items / items_per_page;
        if items % items_per_page != 0 {
            *pages = *pages + 1;
        }
        if type_spec.alloc_items() > 0 {
            // Already allocated this ttype
            return Err(());
        }
        type_spec.set_alloc_items(items);

        let mut last: u16 = 0;
        for i in 0..*pages {
            let page = page_table.top_unused_page + i as u16;
            unsafe {
                invariant(page >= PAGE_MAX as u16);
            }
            //println!("Allocate Page: {}", page);
            //msg!("allocate page: {}", page);
            type_spec.set_page(i, page as u16);
            last = page + 1;
        }
        page_table.top_unused_page = page_table.top_unused_page + *pages as u16;

        let msg = format!("allocate {} - {} items - {} pages - {} total pages", type_id, items, *pages, last);
        msg!(&msg);

        Ok(*pages)
    }

    pub fn len(&mut self, type_id: u16) -> usize {
        let (_p, type_table, _d) = self.parts();
        let type_spec = &type_table[type_id as usize];
        type_spec.alloc_items()
    }

    pub fn index<T: bytemuck::Pod>(&self, type_id: u16, index: usize) -> &T {
        let (_p, type_table, data_pages) = self.parts();
        let type_spec = &type_table[type_id as usize];
        unsafe {
            invariant(index >= type_spec.alloc_items);
        }
        let header_size: usize = type_spec.header_size();
        let offset_size: usize = type_spec.offset_size();
        let inst_size: usize = size_of::<T>();
        let inst_per_page: usize = PAGE_SIZE.checked_sub(header_size + offset_size).unwrap() / inst_size;
        let index_page = index / inst_per_page;
        let index_offset = index % inst_per_page;
        let page_idx = type_spec.get_page(index_page);
        let page_data = &data_pages[page_idx as usize];
        let data_slice = page_data.data::<T>(header_size, offset_size);
        let start = index_offset * inst_size;
        let end = start + inst_size;
        let inst_bytes = &data_slice[start..end];
        //msg!("from_bytes index");
        from_bytes::<T>(inst_bytes)
    }

    pub fn index_mut<T: bytemuck::Pod>(&mut self, type_id: u16, index: usize) -> &mut T {
        let (_p, type_table, data_pages) = self.parts_mut();
        let type_spec = &type_table[type_id as usize];
        unsafe {
            invariant(index >= type_spec.alloc_items);
        }
        let header_size: usize = type_spec.header_size();
        let offset_size: usize = type_spec.offset_size();
        let inst_size: usize = size_of::<T>();
        let inst_per_page: usize = PAGE_SIZE.checked_sub(header_size + offset_size).unwrap() / inst_size;
        let index_page = index / inst_per_page;
        let index_offset = index % inst_per_page;
        let page_idx = type_spec.get_page(index_page);
        let page_data = &mut data_pages[page_idx as usize];
        let data_slice = page_data.data_mut::<T>(header_size, offset_size);
        let start = index_offset * inst_size;
        let end = start + inst_size;
        let inst_bytes = &mut data_slice[start..end];
        //msg!("from_bytes_mut index_mut");
        from_bytes_mut::<T>(inst_bytes)
    }

    pub fn header<H: bytemuck::Pod>(&self, type_id: u16) -> &H {
        let (_p, type_table, data_pages) = self.parts();
        let type_spec = &type_table[type_id as usize];
        let offset_size: usize = type_spec.offset_size();
        let page_idx = type_spec.get_page(0);
        let page_data = &data_pages[page_idx as usize];
        page_data.header::<H>(offset_size)
    }

    pub fn header_mut<H: bytemuck::Pod>(&mut self, type_id: u16) -> &mut H {
        let (_p, type_table, data_pages) = self.parts_mut();
        let type_spec = &type_table[type_id as usize];
        let offset_size: usize = type_spec.offset_size();
        let page_idx = type_spec.get_page(0);
        let page_data = &mut data_pages[page_idx as usize];
        page_data.header_mut::<H>(offset_size)
    }
}

#[cfg(debug_assertions)]
unsafe fn invariant(check: bool) {
    if check {
        unreachable!();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
unsafe fn invariant(check: bool) {
    if check {
        std::hint::unreachable_unchecked();
    }
}

// CritMap: Critbit trees stand-in for hash maps

pub type NodeHandle = u32;

#[derive(IntoPrimitive, TryFromPrimitive)]
#[repr(u32)]
enum NodeTag {
    Uninitialized = 0,
    InnerNode = 1,
    LeafNode = 2,
    FreeNode = 3,
    LastFreeNode = 4,
}

#[derive(Copy, Clone)]
#[repr(packed)]
#[allow(dead_code)]
struct InnerNode {
    tag: u32,
    key: u128,
    prefix_len: u32,
    children: [u32; 2],
}
unsafe impl Zeroable for InnerNode {}
unsafe impl Pod for InnerNode {}

impl InnerNode {
    fn walk_down(&self, search_key: u128) -> (NodeHandle, bool) {
        let crit_bit_mask = (1u128 << 127) >> self.prefix_len;
        let crit_bit = (search_key & crit_bit_mask) != 0;
        (self.children[crit_bit as usize], crit_bit)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
#[repr(packed)]
pub struct LeafNode {
    tag: u32,
    key: u128,
    data: u32,
    _padding: [u32; 2],
}
unsafe impl Zeroable for LeafNode {}
unsafe impl Pod for LeafNode {}

impl LeafNode {
    #[inline]
    pub fn new(
        key: u128,
        data: u32,
    ) -> Self {
        LeafNode {
            tag: NodeTag::LeafNode.into(),
            key,
            data,
            _padding: Zeroable::zeroed(),
        }
    }

    #[inline]
    pub fn key(&self) -> u128 {
        self.key
    }

    #[inline]
    pub fn data(&self) -> u32 {
        self.data
    }
}

#[derive(Copy, Clone)]
#[repr(packed)]
#[allow(dead_code)]
struct FreeNode {
    tag: u32,
    next: u32,
    _padding: [u32; 6],
}
unsafe impl Zeroable for FreeNode {}
unsafe impl Pod for FreeNode {}

const fn _const_max(a: usize, b: usize) -> usize {
    let gt = (a > b) as usize;
    gt * a + (1 - gt) * b
}

const _INNER_NODE_SIZE: usize = size_of::<InnerNode>();
const _LEAF_NODE_SIZE: usize = size_of::<LeafNode>();
const _FREE_NODE_SIZE: usize = size_of::<FreeNode>();
const _NODE_SIZE: usize = 32;

const _INNER_NODE_ALIGN: usize = align_of::<InnerNode>();
const _LEAF_NODE_ALIGN: usize = align_of::<LeafNode>();
const _FREE_NODE_ALIGN: usize = align_of::<FreeNode>();
const _NODE_ALIGN: usize = 1;

const_assert_eq!(_NODE_SIZE, _INNER_NODE_SIZE);
const_assert_eq!(_NODE_SIZE, _LEAF_NODE_SIZE);
const_assert_eq!(_NODE_SIZE, _FREE_NODE_SIZE);

const_assert_eq!(_NODE_ALIGN, _INNER_NODE_ALIGN);
const_assert_eq!(_NODE_ALIGN, _LEAF_NODE_ALIGN);
const_assert_eq!(_NODE_ALIGN, _FREE_NODE_ALIGN);

#[derive(Copy, Clone)]
#[repr(packed)]
#[allow(dead_code)]
pub struct AnyNode {
    tag: u32,
    data: [u32; 7],
}
unsafe impl Zeroable for AnyNode {}
unsafe impl Pod for AnyNode {}

enum NodeRef<'a> {
    Inner(&'a InnerNode),
    Leaf(&'a LeafNode),
}

enum NodeRefMut<'a> {
    Inner(&'a mut InnerNode),
    Leaf(&'a mut LeafNode),
}

impl AnyNode {
    fn key(&self) -> Option<u128> {
        match self.case()? {
            NodeRef::Inner(inner) => Some(inner.key),
            NodeRef::Leaf(leaf) => Some(leaf.key),
        }
    }

    fn prefix_len(&self) -> u32 {
        match self.case().unwrap() {
            NodeRef::Inner(&InnerNode { prefix_len, .. }) => prefix_len,
            NodeRef::Leaf(_) => 128,
        }
    }

    fn children(&self) -> Option<[u32; 2]> {
        match self.case().unwrap() {
            NodeRef::Inner(&InnerNode { children, .. }) => Some(children),
            NodeRef::Leaf(_) => None,
        }
    }

    fn case(&self) -> Option<NodeRef> {
        match NodeTag::try_from(self.tag) {
            Ok(NodeTag::InnerNode) => Some(NodeRef::Inner(cast_ref(self))),
            Ok(NodeTag::LeafNode) => Some(NodeRef::Leaf(cast_ref(self))),
            _ => None,
        }
    }

    fn case_mut(&mut self) -> Option<NodeRefMut> {
        match NodeTag::try_from(self.tag) {
            Ok(NodeTag::InnerNode) => Some(NodeRefMut::Inner(cast_mut(self))),
            Ok(NodeTag::LeafNode) => Some(NodeRefMut::Leaf(cast_mut(self))),
            _ => None,
        }
    }

    #[inline]
    pub fn as_leaf(&self) -> Option<&LeafNode> {
        match self.case() {
            Some(NodeRef::Leaf(leaf_ref)) => Some(leaf_ref),
            _ => None,
        }
    }

    #[inline]
    pub fn as_leaf_mut(&mut self) -> Option<&mut LeafNode> {
        match self.case_mut() {
            Some(NodeRefMut::Leaf(leaf_ref)) => Some(leaf_ref),
            _ => None,
        }
    }
}

impl AsRef<AnyNode> for InnerNode {
    fn as_ref(&self) -> &AnyNode {
        cast_ref(self)
    }
}

impl AsRef<AnyNode> for LeafNode {
    #[inline]
    fn as_ref(&self) -> &AnyNode {
        cast_ref(self)
    }
}

const_assert_eq!(_NODE_SIZE, size_of::<AnyNode>());
const_assert_eq!(_NODE_ALIGN, align_of::<AnyNode>());

#[derive(Copy, Clone)]
pub struct CritMapHeader {
    bump_index: u64,
    free_list_len: u64,
    free_list_head: u32,

    root_node: u32,
    leaf_count: u64,
}
unsafe impl Zeroable for CritMapHeader {}
unsafe impl Pod for CritMapHeader {}

struct CritMapData {}
impl CritMapData {
    fn len(slab: &mut SlabPageAlloc, type_id: u16) -> usize {
        slab.len(type_id)
    }

    fn header(slab: &SlabPageAlloc, type_id: u16) -> &CritMapHeader {
        slab.header::<CritMapHeader>(type_id)
    }

    fn header_mut(slab: &mut SlabPageAlloc, type_id: u16) -> &mut CritMapHeader {
        slab.header_mut::<CritMapHeader>(type_id)
    }

    fn index(slab: &SlabPageAlloc, type_id: u16, idx: usize) -> &AnyNode {
        slab.index::<AnyNode>(type_id, idx)
    }

    fn index_mut(slab: &mut SlabPageAlloc, type_id: u16, idx: usize) -> &mut AnyNode {
        slab.index_mut::<AnyNode>(type_id, idx)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CritMap<'a> {
    pub slab: &'a mut SlabPageAlloc,
    pub type_id: u16,
    pub capacity: u32,
}

pub trait CritMapView<T> {
    fn capacity(&self) -> u64;
    fn clear(&mut self);
    fn is_empty(&self) -> bool;
    fn get(&self, h: NodeHandle) -> Option<&T>;
    fn get_mut(&mut self, h: NodeHandle) -> Option<&mut T>;
    fn insert(&mut self, val: &T) -> Result<u32, ()>;
    fn remove(&mut self, h: NodeHandle) -> Option<T>;
    fn contains(&self, h: NodeHandle) -> bool;
}

impl CritMapView<AnyNode> for CritMap<'_> {
    fn capacity(&self) -> u64 {
        self.capacity as u64
    }

    fn clear(&mut self) {
        *CritMapData::header_mut(self.slab, self.type_id) = CritMapHeader {
            bump_index: 0,
            free_list_len: 0,
            free_list_head: 0,

            root_node: 0,
            leaf_count: 0,
        }
    }

    fn is_empty(&self) -> bool {
        let header: &CritMapHeader = CritMapData::header(self.slab, self.type_id);
        header.bump_index == header.free_list_len
    }

    fn get(&self, key: u32) -> Option<&AnyNode> {
        let node: &AnyNode = CritMapData::index(self.slab, self.type_id, key as usize);
        let tag = NodeTag::try_from(node.tag);
        match tag {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => Some(node),
            _ => None,
        }
    }

    fn get_mut(&mut self, key: u32) -> Option<&mut AnyNode> {
        let node: &mut AnyNode = CritMapData::index_mut(self.slab, self.type_id, key as usize);
        let tag = NodeTag::try_from(node.tag);
        match tag {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => Some(node),
            _ => None,
        }
    }

    fn insert(&mut self, val: &AnyNode) -> Result<u32, ()> {
        match NodeTag::try_from(identity(val.tag)) {
            Ok(NodeTag::InnerNode) | Ok(NodeTag::LeafNode) => (),
            _ => unreachable!(),
        };

        let len = CritMapData::len(self.slab, self.type_id);
        let mut header = *CritMapData::header_mut(self.slab, self.type_id);

        if header.free_list_len == 0 {
            if header.bump_index as usize == len {
                return Err(());
            }

            if header.bump_index == std::u32::MAX as u64 {
                return Err(());
            }
            let key = header.bump_index as u32;
            header.bump_index += 1;

            *CritMapData::header_mut(self.slab, self.type_id) = header;
            *CritMapData::index_mut(self.slab, self.type_id, key as usize) = *val;
            return Ok(key);
        }

        let key = header.free_list_head;
        let node: AnyNode = *CritMapData::index_mut(self.slab, self.type_id, key as usize);

        match NodeTag::try_from(node.tag) {
            Ok(NodeTag::FreeNode) => assert!(header.free_list_len > 1),
            Ok(NodeTag::LastFreeNode) => assert_eq!(identity(header.free_list_len), 1),
            _ => unreachable!(),
        };

        let next_free_list_head: u32;
        {
            let free_list_item: &FreeNode = cast_ref(&node);
            next_free_list_head = free_list_item.next;
        }
        header.free_list_head = next_free_list_head;
        header.free_list_len -= 1;
        *CritMapData::header_mut(self.slab, self.type_id) = header;
        *CritMapData::index_mut(self.slab, self.type_id, key as usize) = *val;
        Ok(key)
    }

    fn remove(&mut self, key: u32) -> Option<AnyNode> {
        let val = *self.get(key)?;
        let mut header = *CritMapData::header_mut(self.slab, self.type_id);
        let mut free_node = FreeNode {
            tag: if header.free_list_len == 0 {
                NodeTag::LastFreeNode.into()
            } else {
                NodeTag::FreeNode.into()
            },
            next: header.free_list_head,
            _padding: Zeroable::zeroed(),
        };
        let any_node: &mut AnyNode = cast_mut(&mut free_node);
        *CritMapData::index_mut(self.slab, self.type_id, key as usize) = *any_node;
        header.free_list_len += 1;
        header.free_list_head = key;
        *CritMapData::header_mut(self.slab, self.type_id) = header;
        Some(val)
    }

    fn contains(&self, key: u32) -> bool {
        self.get(key).is_some()
    }
}

#[derive(Debug)]
pub enum SlabTreeError {
    OutOfSpace,
}

impl CritMap<'_> {
    pub fn str_hash(inp: String) -> u128 {
        murmur3_x86_128(&mut Cursor::new(inp), 0).expect("Hash failed")
    }

    pub fn bytes_hash(inp: &[u8]) -> u128 {
        murmur3_x86_128(&mut Cursor::new(inp), 0).expect("Hash failed")
    }

    fn header(&self) -> &CritMapHeader {
        self.slab.header::<CritMapHeader>(self.type_id)
    }

    fn header_mut(&mut self) -> &mut CritMapHeader {
        self.slab.header_mut::<CritMapHeader>(self.type_id)
    }

    fn root(&self) -> Option<NodeHandle> {
        let header = self.header();
        if header.leaf_count == 0 {
            return None;
        }

        Some(header.root_node)
    }

    fn find_min_max(&self, find_max: bool) -> Option<NodeHandle> {
        let mut root: NodeHandle = self.root()?;
        loop {
            let root_contents = self.get(root).unwrap();
            match root_contents.case().unwrap() {
                NodeRef::Inner(&InnerNode { children, .. }) => {
                    root = children[if find_max { 1 } else { 0 }];
                    continue;
                }
                _ => return Some(root),
            }
        }
    }

    #[inline]
    pub fn find_min(&self) -> Option<NodeHandle> {
        self.find_min_max(false)
    }

    #[inline]
    pub fn find_max(&self) -> Option<NodeHandle> {
        self.find_min_max(true)
    }

    #[inline]
    pub fn insert_leaf(
        &mut self,
        new_leaf: &LeafNode,
    ) -> Result<(NodeHandle, Option<LeafNode>), SlabTreeError> {
        let mut root: NodeHandle = match self.root() {
            Some(h) => h,
            None => {
                // create a new root if none exists
                match self.insert(new_leaf.as_ref()) {
                    Ok(handle) => {
                        self.header_mut().root_node = handle;
                        self.header_mut().leaf_count = 1;
                        return Ok((handle, None));
                    }
                    Err(()) => return Err(SlabTreeError::OutOfSpace),
                }
            }
        };
        loop {
            // check if the new node will be a child of the root
            let root_contents = *self.get(root).unwrap();
            let root_key = root_contents.key().unwrap();
            if root_key == new_leaf.key {
                if let Some(NodeRef::Leaf(&old_root_as_leaf)) = root_contents.case() {
                    // clobber the existing leaf
                    *self.get_mut(root).unwrap() = *new_leaf.as_ref();
                    return Ok((root, Some(old_root_as_leaf)));
                }
            }
            let shared_prefix_len: u32 = (root_key ^ new_leaf.key).leading_zeros();
            match root_contents.case() {
                None => unreachable!(),
                Some(NodeRef::Inner(inner)) => {
                    let keep_old_root = shared_prefix_len >= inner.prefix_len;
                    if keep_old_root {
                        root = inner.walk_down(new_leaf.key).0;
                        continue;
                    };
                }
                _ => (),
            };

            // change the root in place to represent the LCA of [new_leaf] and [root]
            let crit_bit_mask: u128 = (1u128 << 127) >> shared_prefix_len;
            let new_leaf_crit_bit = (crit_bit_mask & new_leaf.key) != 0;
            let old_root_crit_bit = !new_leaf_crit_bit;

            let new_leaf_handle = self
                .insert(new_leaf.as_ref())
                .map_err(|()| SlabTreeError::OutOfSpace)?;
            let moved_root_handle = match self.insert(&root_contents) {
                Ok(h) => h,
                Err(()) => {
                    self.remove(new_leaf_handle).unwrap();
                    return Err(SlabTreeError::OutOfSpace);
                }
            };

            let new_root: &mut InnerNode = cast_mut(self.get_mut(root).unwrap());
            *new_root = InnerNode {
                tag: NodeTag::InnerNode.into(),
                prefix_len: shared_prefix_len,
                key: new_leaf.key,
                children: [0; 2],
                //_padding: Zeroable::zeroed(),
            };

            new_root.children[new_leaf_crit_bit as usize] = new_leaf_handle;
            new_root.children[old_root_crit_bit as usize] = moved_root_handle;
            self.header_mut().leaf_count += 1;
            return Ok((new_leaf_handle, None));
        }
    }

    pub fn get_key(&self, search_key: u128) -> Option<&LeafNode> {
        let mut node_handle: NodeHandle = self.root()?;
        loop {
            let node_ref = self.get(node_handle).unwrap();
            let node_prefix_len = node_ref.prefix_len();
            let node_key = node_ref.key().unwrap();
            let common_prefix_len = (search_key ^ node_key).leading_zeros();
            if common_prefix_len < node_prefix_len {
                return None;
            }
            match node_ref.case().unwrap() {
                NodeRef::Leaf(_) => break Some(node_ref.as_leaf().unwrap()),
                NodeRef::Inner(inner) => {
                    let crit_bit_mask = (1u128 << 127) >> node_prefix_len;
                    let _search_key_crit_bit = (search_key & crit_bit_mask) != 0;
                    node_handle = inner.walk_down(search_key).0;
                    continue;
                }
            }
        }
    }

    pub fn find_by_key(&self, search_key: u128) -> Option<NodeHandle> {
        let mut node_handle: NodeHandle = self.root()?;
        loop {
            let node_ref = self.get(node_handle).unwrap();
            let node_prefix_len = node_ref.prefix_len();
            let node_key = node_ref.key().unwrap();
            let common_prefix_len = (search_key ^ node_key).leading_zeros();
            if common_prefix_len < node_prefix_len {
                return None;
            }
            match node_ref.case().unwrap() {
                NodeRef::Leaf(_) => break Some(node_handle),
                NodeRef::Inner(inner) => {
                    let crit_bit_mask = (1u128 << 127) >> node_prefix_len;
                    let _search_key_crit_bit = (search_key & crit_bit_mask) != 0;
                    node_handle = inner.walk_down(search_key).0;
                    continue;
                }
            }
        }
    }

    /* pub(crate) fn find_by<F: Fn(&LeafNode) -> bool>(
        &self,
        limit: &mut u16,
        predicate: F,
    ) -> Vec<u128> {
        let mut found = Vec::new();
        let mut nodes_to_search: Vec<NodeHandle> = Vec::new();
        let mut current_node: Option<&AnyNode>;

        let top_node = self.root();

        // No found nodes.
        if top_node.is_none() {
            return found;
        }

        nodes_to_search.push(top_node.unwrap());

        // Search through the tree.
        while !nodes_to_search.is_empty() && *limit > 0 {
            *limit -= 1;

            current_node = self.get(nodes_to_search.pop().unwrap());

            // Node not found.
            if current_node.is_none() {
                break;
            }

            match current_node.unwrap().case().unwrap() {
                NodeRef::Leaf(leaf) if predicate(leaf) => {
                    // Found a matching leaf.
                    found.push(leaf.key)
                }
                NodeRef::Inner(inner) => {
                    // Search the children.
                    nodes_to_search.push(inner.children[0]);
                    nodes_to_search.push(inner.children[1]);
                }
                _ => (),
            }
        }

        found
    } */

    #[inline]
    pub fn remove_by_key(&mut self, search_key: u128) -> Option<LeafNode> {
        let mut parent_h = self.root()?;
        let mut child_h;
        let mut crit_bit;
        match self.get(parent_h).unwrap().case().unwrap() {
            NodeRef::Leaf(&leaf) if leaf.key == search_key => {
                let header = self.header_mut();
                assert_eq!(identity(header.leaf_count), 1);
                header.root_node = 0;
                header.leaf_count = 0;
                let _old_root = self.remove(parent_h).unwrap();
                return Some(leaf);
            }
            NodeRef::Leaf(_) => return None,
            NodeRef::Inner(inner) => {
                let (ch, cb) = inner.walk_down(search_key);
                child_h = ch;
                crit_bit = cb;
            }
        }
        loop {
            match self.get(child_h).unwrap().case().unwrap() {
                NodeRef::Inner(inner) => {
                    let (grandchild_h, grandchild_crit_bit) = inner.walk_down(search_key);
                    parent_h = child_h;
                    child_h = grandchild_h;
                    crit_bit = grandchild_crit_bit;
                    continue;
                }
                NodeRef::Leaf(&leaf) => {
                    if leaf.key != search_key {
                        return None;
                    }

                    break;
                }
            }
        }
        // replace parent with its remaining child node
        // free child_h, replace *parent_h with *other_child_h, free other_child_h
        let other_child_h = self.get(parent_h).unwrap().children().unwrap()[!crit_bit as usize];
        let other_child_node_contents = self.remove(other_child_h).unwrap();
        *self.get_mut(parent_h).unwrap() = other_child_node_contents;
        self.header_mut().leaf_count -= 1;
        Some(cast(self.remove(child_h).unwrap()))
    }

    #[inline]
    pub fn remove_min(&mut self) -> Option<LeafNode> {
        self.remove_by_key(self.get(self.find_min()?)?.key()?)
    }

    #[inline]
    pub fn remove_max(&mut self) -> Option<LeafNode> {
        self.remove_by_key(self.get(self.find_max()?)?.key()?)
    }

    pub fn traverse(&self) -> Vec<&LeafNode> {
        fn walk_rec<'a>(crit: &'a CritMap, sub_root: NodeHandle, buf: &mut Vec<&'a LeafNode>) {
            match crit.get(sub_root).unwrap().case().unwrap() {
                NodeRef::Leaf(leaf) => {
                    buf.push(leaf);
                }
                NodeRef::Inner(inner) => {
                    walk_rec(crit, inner.children[0], buf);
                    walk_rec(crit, inner.children[1], buf);
                }
            }
        }

        let mut buf = Vec::with_capacity(self.header().leaf_count as usize);
        if let Some(r) = self.root() {
            walk_rec(self, r, &mut buf);
        }
        assert_eq!(buf.len(), buf.capacity());
        buf
    }

/*    #[cfg(test)]
    fn hexdump(&self) {
        println!("Header:");
        hexdump::hexdump(bytemuck::bytes_of(self.header()));
        println!("Data:");
        hexdump::hexdump(cast_slice(self.nodes()));
    } */

/*    #[cfg(test)]
    fn check_invariants(&self) {
        // first check the live tree contents
        let mut count = 0;
        fn check_rec(
            slab: &Slab,
            key: NodeHandle,
            last_prefix_len: u32,
            last_prefix: u128,
            last_crit_bit: bool,
            count: &mut u64,
        ) {
            *count += 1;
            let node = slab.get(key).unwrap();
            assert!(node.prefix_len() > last_prefix_len);
            let node_key = node.key().unwrap();
            assert_eq!(
                last_crit_bit,
                (node_key & ((1u128 << 127) >> last_prefix_len)) != 0
            );
            let prefix_mask = (((((1u128) << 127) as i128) >> last_prefix_len) as u128) << 1;
            assert_eq!(last_prefix & prefix_mask, node.key().unwrap() & prefix_mask);
            if let Some([c0, c1]) = node.children() {
                check_rec(slab, c0, node.prefix_len(), node_key, false, count);
                check_rec(slab, c1, node.prefix_len(), node_key, true, count);
            }
        }
        if let Some(root) = self.root() {
            count += 1;
            let node = self.get(root).unwrap();
            let node_key = node.key().unwrap();
            if let Some([c0, c1]) = node.children() {
                check_rec(self, c0, node.prefix_len(), node_key, false, &mut count);
                check_rec(self, c1, node.prefix_len(), node_key, true, &mut count);
            }
        }
        assert_eq!(
            count + self.header().free_list_len as u64,
            identity(self.header().bump_index)
        );

        let mut free_nodes_remaining = self.header().free_list_len;
        let mut next_free_node = self.header().free_list_head;
        loop {
            let contents;
            match free_nodes_remaining {
                0 => break,
                1 => {
                    contents = &self.nodes()[next_free_node as usize];
                    assert_eq!(identity(contents.tag), u32::from(NodeTag::LastFreeNode));
                }
                _ => {
                    contents = &self.nodes()[next_free_node as usize];
                    assert_eq!(identity(contents.tag), u32::from(NodeTag::FreeNode));
                }
            };
            let typed_ref: &FreeNode = cast_ref(contents);
            next_free_node = typed_ref.next;
            free_nodes_remaining -= 1;
        }
    } */
}

// SlabVec

#[derive(Copy, Clone)]
#[repr(packed)]
pub struct SlabVec {
    next_index: u32,
}
unsafe impl Zeroable for SlabVec {}
unsafe impl Pod for SlabVec {}

impl SlabVec {
    #[inline]
    pub fn new() -> Self {
        SlabVec {
            next_index: 0,
        }
    }

    pub fn next_index(&mut self) -> u32 {
        let next = self.next_index;
        self.next_index = self.next_index.checked_add(1).expect("Overflow");
        next
    }

    pub fn len(&self) -> u32 {
        self.next_index as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    //use bytemuck::bytes_of;
    //use rand::prelude::*;

    #[test]
    fn simulate_slab_page_alloc() {

        let size = PAGE_TABLE_SIZE;
        println!("Total Size: {} Bytes", size);

        let mut aligned_pgs = vec![0u64; 1_000_000];
        let bytes_pgs: &mut [u8] = cast_slice_mut(aligned_pgs.as_mut_slice());

        println!("Begin Allocation");
        let pt = SlabPageAlloc::new(bytes_pgs);
        pt.allocate::<SlabVec, DataNode>(0, 10);
        pt.allocate::<CritMapHeader, AnyNode>(1, 100);

        let mut slv = SlabVec::new();
        *pt.header_mut::<SlabVec>(0) = slv;

        //let head: &DataNode = pt.header::<DataNode>(0);
        //println!("Head0 {} {}", head.data()[0], head.data()[1]);

        for i in 0..10 {
            let j: u64 = i as u64;
            *pt.index_mut::<DataNode>(0, slv.next_index() as usize) = DataNode::new([j * 2, j + 1]);
            println!("Set0 {}", i);
        }

        for i in 0..slv.len() {
            let data: &DataNode = pt.index::<DataNode>(0, i as usize);
            println!("Get0 {} {} {}", i, data.data()[0], data.data()[1]);
        }

        let mut cm = CritMap {
            slab: pt,
            type_id: 1,
            capacity: 100,
        };

        for k in 0u32..10u32 {
            let s = fmt::format(format_args!("Hello, {}!", k));
            let node = LeafNode::new(CritMap::str_hash(s), k as u32);
            cm.insert_leaf(&node);
        }

        let s1 = fmt::format(format_args!("Hello, {}!", 3));
        cm.remove_by_key(CritMap::bytes_hash(s1.as_bytes()));
        let s2 = fmt::format(format_args!("Hello, {}!", 8));
        cm.remove_by_key(CritMap::str_hash(s2));

        let leafs: Vec<&LeafNode> = cm.traverse();
        for i in leafs {
            let istr: String = i.key().to_string();
            println!("Leaf ID: {} {}", istr, i.data().to_string());
        }
    }
}
