use std::path::{Path, PathBuf, Component, Components};
use std::fmt::Debug;
use std::io::{Read, Write, Seek, SeekFrom, Result};
use std::io::{Error, ErrorKind};

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::RwLock;
use std::ops::{Deref, DerefMut};

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use std::cmp;

use vfs::{VFS, VPath, VMetadata};

pub type Filename = String;

#[derive(Debug, Clone)]
pub struct DataHandle(Arc<RwLock<Vec<u8>>>);

impl DataHandle {
    fn new() -> DataHandle {
        DataHandle(Arc::new(RwLock::new(Vec::new())))
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum NodeKind {
    Directory,
    File,
}

#[derive(Debug)]
struct FsNode {
    kind: NodeKind,
    pub children: HashMap<String, FsNode>,
    pub data: DataHandle,
}

impl FsNode {
    pub fn new_directory() -> Self {
        FsNode {
            kind: NodeKind::Directory,
            children: HashMap::new(),
            data: DataHandle::new(),
        }
    }

    pub fn new_file() -> Self {
        FsNode {
            kind: NodeKind::File,
            children: HashMap::new(),
            data: DataHandle::new(),
        }
    }

    fn metadata(&mut self) -> MemoryMetadata {
        MemoryMetadata {
            kind: self.kind.clone(),
            len: self.data.0.read().unwrap().len() as u64,
        }
    }
}

#[derive(Debug)]
pub struct MemoryFSImpl {
    root: FsNode,
}

pub type MemoryFSHandle = Arc<RwLock<MemoryFSImpl>>;

/// An ephemeral in-memory file system, intended mainly for unit tests
#[derive(Debug)]
pub struct MemoryFS {
    handle: MemoryFSHandle,
}

impl MemoryFS {
    pub fn new() -> MemoryFS {
        MemoryFS { handle: Arc::new(RwLock::new(MemoryFSImpl { root: FsNode::new_directory() })) }
    }
}


#[derive(Debug)]
pub struct MemoryFile {
    pub data: DataHandle,
    pub pos: u64,
}

impl Read for MemoryFile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = try!((&self.data.0.write().unwrap().deref()[self.pos as usize..]).read(buf));
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for MemoryFile {
    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        let mut guard = self.data.0.write().unwrap();
        let ref mut vec: &mut Vec<u8> = guard.deref_mut();
        // From cursor.rs
        let pos = self.pos;
        let len = vec.len();
        let amt = pos.saturating_sub(len as u64);
        vec.resize(len + amt as usize, 0);
        {
            let pos = pos as usize;
            let space = vec.len() - pos;
            let (left, right) = buf.split_at(cmp::min(space, buf.len()));
            vec[pos..pos + left.len()].clone_from_slice(left);
            vec.extend_from_slice(right);
        }

        // Bump us forward
        self.pos = pos + buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> Result<()> {
        // Nothing to do
        Ok(())
    }
}

impl Seek for MemoryFile {
    fn seek(&mut self, style: SeekFrom) -> Result<u64> {
        let pos = match style {
            SeekFrom::Start(n) => {
                self.pos = n;
                return Ok(n);
            }
            SeekFrom::End(n) => self.data.0.read().unwrap().len() as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };

        if pos < 0 {
            Err(Error::new(ErrorKind::InvalidInput,
                           "invalid seek to a negative position"))
        } else {
            self.pos = pos as u64;
            Ok(self.pos)
        }
    }
}

pub struct MemoryMetadata {
    kind: NodeKind,
    len: u64,
}

impl VMetadata for MemoryMetadata {
    fn is_dir(&self) -> bool {
        self.kind == NodeKind::Directory
    }
    fn is_file(&self) -> bool {
        self.kind == NodeKind::File
    }
    fn len(&self) -> u64 {
        self.len
    }
}

impl VFS for MemoryFS {
    type PATH = MemoryPath;
    type FILE = MemoryFile;
    type METADATA = MemoryMetadata;

    fn path<T: Into<String>>(&self, path: T) -> MemoryPath {
        MemoryPath::new(&self.handle, path.into())
    }
}


#[derive(Debug, Clone)]
pub struct MemoryPath {
    pub path: Filename,
    fs: MemoryFSHandle,
}

impl MemoryPath {
    pub fn new(fs: &MemoryFSHandle, path: Filename) -> Self {
        return MemoryPath {
            path: path,
            fs: fs.clone(),
        };
    }

    fn with_node<R, F: FnOnce(&mut FsNode) -> R>(&self, f: F) -> Result<R> {
        let root = &mut self.fs.write().unwrap().root;
        let mut components: Vec<&str> = self.path.split("/").collect();
        components.reverse();
        components.pop();
        return traverse_with(root, &mut components, f);
    }

    pub fn decompose_path(&self) -> (Option<String>, String) {
        let mut split = self.path.rsplitn(2, "/");
        if let Some(mut filename) = split.next() {
            if let Some(mut parent) = split.next() {
                if parent.is_empty() {
                    parent = "/";
                }
                if filename.is_empty() {
                    filename = parent;
                    return (None, filename.to_owned());
                }
                return (Some(parent.to_owned()), filename.to_owned());
            }
        }
        return (None, self.path.clone());
    }
}

fn traverse_mkdir(node: &mut FsNode, components: &mut Vec<&str>) -> Result<()> {
    if let Some(component) = components.pop() {
        let directory = &mut node.children
                                 .entry(component.to_owned())
                                 .or_insert_with(FsNode::new_directory);
        traverse_mkdir(directory, components)
    } else {
        Ok(())
    }
}

fn traverse_with<R, F: FnOnce(&mut FsNode) -> R>(node: &mut FsNode,
                                                 components: &mut Vec<&str>,
                                                 f: F)
                                                 -> Result<R> {
    if let Some(component) = components.pop() {
        if component.is_empty() {
            return traverse_with(node, components, f);
        }
        let entry = node.children.get_mut(component);
        if let Some(directory) = entry {
            return traverse_with(directory, components, f);
        } else {
            return Err(Error::new(ErrorKind::Other, format!("File not found {:?}", component)));
        }
    } else {
        Ok(f(node))
    }
}

impl VPath for MemoryPath {
    type FS = MemoryFS;

    fn open(&self) -> Result<MemoryFile> {
        let data = self.with_node(|node| node.data.clone()).unwrap();
        Ok(MemoryFile {
            data: data,
            pos: 0,
        })
    }

    fn create(&self) -> Result<MemoryFile> {
        let parent_path = self.parent().unwrap();
        let data = try!(parent_path.with_node(|node| {
            let file_node = node.children
                                .entry(self.file_name().unwrap())
                                .or_insert_with(FsNode::new_file);
            // TODO: check not directory
            return file_node.data.clone();
        }));
        data.0.write().unwrap().clear();
        Ok(MemoryFile {
            data: data,
            pos: 0,
        })
    }

    fn append(&self) -> Result<MemoryFile> {
        let parent_path = self.parent().unwrap();
        let data = try!(parent_path.with_node(|node| {
            let file_node = node.children
                                .entry(self.file_name().unwrap())
                                .or_insert_with(FsNode::new_file);
            // TODO: check not directory
            return file_node.data.clone();
        }));
        let len = data.0.read().unwrap().len();
        Ok(MemoryFile {
            data: data,
            pos: len as u64,
        })
    }


    fn parent(&self) -> Option<MemoryPath> {
        self.decompose_path().0.map(|parent| MemoryPath::new(&self.fs.clone(), parent))
    }


    fn file_name(&self) -> Option<String> {
        Some(self.decompose_path().1)
    }

    fn push<'a, T: Into<&'a str>>(&mut self, path: T) {
        // TODO: sanity checks
        if !self.path.ends_with('/') {
            self.path.push_str("/");
        }
        self.path.push_str(&path.into());
    }


    fn mkdir(&self) -> Result<()> {
        let root = &mut self.fs.write().unwrap().root;
        let mut components: Vec<&str> = self.path.split("/").collect();
        components.reverse();
        components.pop();
        traverse_mkdir(root, &mut components)
    }

    fn exists(&self) -> bool {
        return self.with_node(|node| ()).is_ok();
    }

    fn metadata(&self) -> Result<MemoryMetadata> {
        return self.with_node(FsNode::metadata);
    }

    fn read_dir(&self) -> Result<Box<Iterator<Item = Result<MemoryPath>>>> {
        let children = try!(self.with_node(|node| {
            let children: Vec<_> = node.children.keys().map(|name| {
                Ok(MemoryPath::new(&self.fs, self.path.clone() + "/" + name))
            }).collect();
            return Box::new(children.into_iter());
        }));
        return Ok(children);
    }

}


impl<'a> From<&'a MemoryPath> for String {
    fn from(path: &'a MemoryPath) -> String {
        path.path.clone()
    }
}

impl PartialEq for MemoryPath {
    fn eq(&self, other: &MemoryPath) -> bool {
        self.path == other.path
    }
}




#[cfg(test)]
mod tests {
    use std::io::{Read, Write, Seek, SeekFrom, Result};

    use super::*;
    use VPath;
    use vfs::{VFS, VMetadata};

    #[test]
    fn mkdir() {
        let fs = MemoryFS::new();
        let path = fs.path("/foo/bar/baz");
        assert!(!path.exists(), "Path should not exist");
        path.mkdir().unwrap();
        assert!(path.exists(), "Path should exist now");
        assert!(path.metadata().unwrap().is_dir(), "Path should be dir");
        assert!(!path.metadata().unwrap().is_file(),
                "Path should be not be a file");
        assert!(path.metadata().unwrap().len() == 0, "Path size should be 0");
    }

    #[test]
    fn read_empty_file() {
        let fs = MemoryFS::new();
        let path = fs.path("/foobar.txt");
        path.create().unwrap();
        let mut file = path.open().unwrap();
        let mut string: String = "".to_owned();
        file.read_to_string(&mut string).unwrap();
        assert_eq!(string, "");
    }

    #[test]
    fn write_and_read_file() {
        let fs = MemoryFS::new();
        let path = fs.path("/foobar.txt");
        {
            let mut file = path.create().unwrap();
            write!(file, "Hello world").unwrap();
            write!(file, "!").unwrap();
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "Hello world!");
        }
        {
            let mut file = path.open().unwrap();
            file.seek(SeekFrom::Start(1)).unwrap();
            write!(file, "a").unwrap();
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "Hallo world!");
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.seek(SeekFrom::End(-1)).unwrap();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "!");
        }
        {
            let file = path.create().unwrap();
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "");
        }
    }

    #[test]
    fn append() {
        let fs = MemoryFS::new();
        let path = fs.path("/foobar.txt");
        {
            let mut file = path.append().unwrap();
            write!(file, "Hello").unwrap();
            write!(file, " world").unwrap();
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "Hello world");
        }
        {
            let mut file = path.append().unwrap();
            write!(file, "!").unwrap();
        }
        {
            let mut file = path.open().unwrap();
            let mut string: String = "".to_owned();
            file.read_to_string(&mut string).unwrap();
            assert_eq!(string, "Hello world!");
        }
    }

    #[test]
    fn push() {
        let fs = MemoryFS::new();
        let mut path = fs.path("/");
        let mut path2 = path.clone();
        assert_eq!(String::from(&path), "/");
        path.push("foo");
        assert_eq!(String::from(&path), "/foo");
        path.push("bar");
        assert_eq!(String::from(&path), "/foo/bar");

        assert_eq!(String::from(&path2), "/");
        path2.push("foo/bar");
        assert_eq!(String::from(&path2), "/foo/bar");
    }

    #[test]
    fn parent() {
        let fs = MemoryFS::new();
        let path = fs.path("/foo");
        let path2 = fs.path("/foo/bar");
        assert_eq!(path2.parent().unwrap(), path);
        assert_eq!(String::from(&path.parent().unwrap()), "/");
        assert_eq!(fs.path("/").parent(), None);
    }

    #[test]
    fn read_dir() {
        let fs = MemoryFS::new();
        let path = fs.path("/foo");
        let path2 = fs.path("/foo/bar");
        let path3 = fs.path("/foo/baz");
        path2.mkdir().unwrap();
        path3.create().unwrap();
        let mut entries: Vec<String> = path.read_dir().unwrap().map(Result::unwrap).map(|x| x.path.clone()).collect();
        entries.sort();
        assert_eq!(entries, vec!["/foo/bar".to_owned(), "/foo/baz".to_owned()]);
    }


}
