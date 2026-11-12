use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

const BASE_APP_ID: i32 = 1875830;
const DUMMY_APP_ID: i32 = 9999999;
const VOICE_TAGS: [&str; 2] = ["japanese_voice", "english_voice"];
const MOUNT: &str = "../../../Project/Content/";
const DIR: &str = "Blueprints/DLC/";
const FILES: [&str; 2] = ["DlcTable.uasset", "DlcTable.uexp"];
const MAGIC: u32 = 0x5A6F12E1;
const FOOTER_LEN: u64 = 221;
const OUT_NAME: &str = "zz_GhostDLCFix_P.pak";

type R<T> = Result<T, String>;

struct Cur<'a> {
    b: &'a [u8],
    o: usize,
}

impl<'a> Cur<'a> {
    fn new(b: &'a [u8], o: usize) -> Self {
        Cur { b, o }
    }
    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let s = self.b.get(self.o..self.o + n).ok_or("données tronquées")?;
        self.o += n;
        Ok(s)
    }
    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> R<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn var(&mut self, small: bool) -> R<u64> {
        if small { Ok(self.u32()? as u64) } else { self.u64() }
    }
    fn fstring(&mut self) -> R<String> {
        let n = self.i32()?;
        if n < 0 {
            let raw = self.take(n.unsigned_abs() as usize * 2)?;
            let w: Vec<u16> = raw.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            Ok(String::from_utf16_lossy(&w[..w.len().saturating_sub(1)]))
        } else {
            let raw = self.take(n as usize)?;
            Ok(String::from_utf8_lossy(&raw[..raw.len().saturating_sub(1)]).into_owned())
        }
    }
}

struct Entry {
    offset: u64,
    size: u64,
    method: u32,
    block_size: u64,
    blocks: Vec<(u64, u64)>,
}

struct Pak {
    file: File,
    methods: Vec<String>,
    entries: HashMap<String, Entry>,
}

fn read_at(f: &mut File, off: u64, len: u64) -> R<Vec<u8>> {
    let mut v = vec![0u8; len as usize];
    f.seek(SeekFrom::Start(off)).map_err(|e| e.to_string())?;
    f.read_exact(&mut v).map_err(|e| e.to_string())?;
    Ok(v)
}

fn decode_entry(enc: &[u8], loc: usize) -> R<Entry> {
    let mut c = Cur::new(enc, loc);
    let v = c.u32()?;
    let block_size = match v & 0x3f {
        0x3f => c.u32()? as u64,
        b => (b as u64) << 11,
    };
    let count = ((v >> 6) & 0xffff) as u64;
    let method = (v >> 23) & 0x3f;
    if (v >> 22) & 1 == 1 {
        return Err("entrée chiffrée non supportée".into());
    }
    let offset = c.var(v >> 31 & 1 == 1)?;
    let size = c.var(v >> 30 & 1 == 1)?;
    let csize = if method != 0 { c.var(v >> 29 & 1 == 1)? } else { size };
    let mut blocks = Vec::new();
    if method != 0 {
        let header = 8 * 3 + 4 + 20 + 4 + 16 * count + 1 + 4;
        if count == 1 {
            blocks.push((header, header + csize));
        } else {
            let mut s = header;
            for _ in 0..count {
                let n = c.u32()? as u64;
                blocks.push((s, s + n));
                s += n;
            }
        }
    }
    Ok(Entry { offset, size, method, block_size, blocks })
}

fn open_pak(path: &Path) -> R<Pak> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    if len < FOOTER_LEN {
        return Err(format!("{}: fichier trop petit", path.display()));
    }
    let footer = read_at(&mut file, len - FOOTER_LEN, FOOTER_LEN)?;
    let mut c = Cur::new(&footer, 17);
    if c.u32()? != MAGIC || c.i32()? != 11 {
        return Err(format!("{}: format de pak inattendu", path.display()));
    }
    let (index_off, index_len) = (c.u64()?, c.u64()?);
    c.take(20)?;
    let methods = (0..5)
        .map(|_| Ok(String::from_utf8_lossy(c.take(32)?).trim_end_matches('\0').to_string()))
        .collect::<R<Vec<_>>>()?;
    let index = read_at(&mut file, index_off, index_len)?;
    let mut c = Cur::new(&index, 0);
    let mount = c.fstring()?;
    c.take(12)?;
    if c.u32()? != 0 {
        c.take(36)?;
    }
    if c.u32()? == 0 {
        return Err("index de répertoires absent".into());
    }
    let (fdi_off, fdi_len) = (c.u64()?, c.u64()?);
    c.take(20)?;
    let enc_len = c.i32()? as usize;
    let enc = c.take(enc_len)?;
    let fdi = read_at(&mut file, fdi_off, fdi_len)?;
    let mut d = Cur::new(&fdi, 0);
    let mut entries = HashMap::new();
    for _ in 0..d.i32()? {
        let dir = d.fstring()?;
        for _ in 0..d.i32()? {
            let name = d.fstring()?;
            let loc = d.i32()?;
            if loc >= 0 {
                entries.insert(format!("{mount}{dir}{name}"), decode_entry(enc, loc as usize)?);
            }
        }
    }
    Ok(Pak { file, methods, entries })
}

fn extract(pak: &mut Pak, path: &str) -> R<Vec<u8>> {
    let e = pak.entries.get(path).ok_or(format!("{path} introuvable dans le pak"))?;
    let (offset, size, method, block_size, blocks) = (e.offset, e.size, e.method, e.block_size, e.blocks.clone());
    if method == 0 {
        return read_at(&mut pak.file, offset + 53, size);
    }
    let name = pak.methods.get(method as usize - 1).cloned().unwrap_or_default();
    if name != "Oodle" {
        return Err(format!("compression {name} non supportée"));
    }
    let mut out = Vec::with_capacity(size as usize);
    for (s, end) in blocks {
        let src = read_at(&mut pak.file, offset + s, end - s)?;
        let mut dst = vec![0u8; block_size.min(size - out.len() as u64) as usize];
        let n = oozextract::Extractor::new()
            .read_from_slice(&src, &mut dst)
            .map_err(|e| format!("Oodle: {e:?}"))?;
        out.extend_from_slice(&dst[..n]);
    }
    if out.len() as u64 != size {
        return Err(format!("{path}: taille décompressée incorrecte"));
    }
    Ok(out)
}

fn name_map(uasset: &[u8]) -> R<Vec<String>> {
    let mut c = Cur::new(uasset, 20);
    let custom = c.i32()? as usize;
    c.take(custom * 20 + 4)?;
    c.fstring()?;
    c.take(4)?;
    let (count, offset) = (c.i32()?, c.i32()? as usize);
    let mut c = Cur::new(uasset, offset);
    (0..count)
        .map(|_| {
            let s = c.fstring()?;
            c.take(4)?;
            Ok(s)
        })
        .collect()
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|p| p + from)
}

fn patch_uexp(names: &[String], uexp: &mut [u8]) -> R<()> {
    let fname = |n: &str| -> R<Vec<u8>> {
        let i = names.iter().position(|x| x == n).ok_or(format!("nom {n} absent"))? as i32;
        Ok([i.to_le_bytes(), 0i32.to_le_bytes()].concat())
    };
    let pattern = [fname("AppID")?, fname("IntProperty")?, 4i32.to_le_bytes().to_vec(), vec![0; 5]].concat();
    for tag in VOICE_TAGS {
        let row = find(uexp, &fname(tag)?, 0).ok_or(format!("ligne {tag} introuvable"))?;
        let at = find(uexp, &pattern, row).ok_or(format!("AppID de {tag} introuvable"))? + pattern.len();
        if find(&uexp[..at], &fname("NameTag")?, row + 8).is_some() || at + 4 > uexp.len() {
            return Err("structure de DlcTable inattendue".into());
        }
        let value = i32::from_le_bytes(uexp[at..at + 4].try_into().unwrap());
        if value != DUMMY_APP_ID {
            return Err(format!("AppID inattendu pour {tag} : {value}"));
        }
        uexp[at..at + 4].copy_from_slice(&BASE_APP_ID.to_le_bytes());
    }
    Ok(())
}

fn put_fstring(v: &mut Vec<u8>, s: &str) {
    v.extend_from_slice(&(s.len() as i32 + 1).to_le_bytes());
    v.extend_from_slice(s.as_bytes());
    v.push(0);
}

fn sha1(b: &[u8]) -> [u8; 20] {
    sha1_smol::Sha1::from(b).digest().bytes()
}

fn build_pak(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = Vec::new();
    let mut fdi = Vec::new();
    fdi.extend_from_slice(&3i32.to_le_bytes());
    put_fstring(&mut fdi, "/");
    fdi.extend_from_slice(&0i32.to_le_bytes());
    put_fstring(&mut fdi, "Blueprints/");
    fdi.extend_from_slice(&0i32.to_le_bytes());
    put_fstring(&mut fdi, DIR);
    fdi.extend_from_slice(&(files.len() as i32).to_le_bytes());
    for (name, data) in files {
        put_fstring(&mut fdi, name);
        fdi.extend_from_slice(&(enc.len() as i32).to_le_bytes());
        enc.extend_from_slice(&0xE000_0000u32.to_le_bytes());
        enc.extend_from_slice(&(out.len() as u32).to_le_bytes());
        enc.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&(data.len() as u64).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&sha1(data));
        out.extend_from_slice(&[0; 5]);
        out.extend_from_slice(data);
    }
    let fdi_off = out.len() as u64;
    out.extend_from_slice(&fdi);
    let mut index = Vec::new();
    put_fstring(&mut index, MOUNT);
    index.extend_from_slice(&(files.len() as i32).to_le_bytes());
    index.extend_from_slice(&0u64.to_le_bytes());
    index.extend_from_slice(&0u32.to_le_bytes());
    index.extend_from_slice(&1u32.to_le_bytes());
    index.extend_from_slice(&fdi_off.to_le_bytes());
    index.extend_from_slice(&(fdi.len() as u64).to_le_bytes());
    index.extend_from_slice(&sha1(&fdi));
    index.extend_from_slice(&(enc.len() as i32).to_le_bytes());
    index.extend_from_slice(&enc);
    index.extend_from_slice(&0i32.to_le_bytes());
    let index_off = out.len() as u64;
    out.extend_from_slice(&index);
    out.extend_from_slice(&[0; 17]);
    out.extend_from_slice(&MAGIC.to_le_bytes());
    out.extend_from_slice(&11i32.to_le_bytes());
    out.extend_from_slice(&index_off.to_le_bytes());
    out.extend_from_slice(&(index.len() as u64).to_le_bytes());
    out.extend_from_slice(&sha1(&index));
    out.extend_from_slice(&[0; 160]);
    out
}

fn run(game: &Path) -> R<PathBuf> {
    let paks = game.join("Project/Content/Paks");
    let mut pak = open_pak(&paks.join("pakchunk0-WindowsNoEditor.pak"))?;
    let uasset = extract(&mut pak, &format!("{MOUNT}{DIR}{}", FILES[0]))?;
    let mut uexp = extract(&mut pak, &format!("{MOUNT}{DIR}{}", FILES[1]))?;
    patch_uexp(&name_map(&uasset)?, &mut uexp)?;
    let files = [(FILES[0], uasset), (FILES[1], uexp)];
    let mods = paks.join("~mods");
    fs::create_dir_all(&mods).map_err(|e| e.to_string())?;
    let out = mods.join(OUT_NAME);
    fs::write(&out, build_pak(&files)).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut check = open_pak(&out)?;
    for (name, data) in &files {
        if extract(&mut check, &format!("{MOUNT}{DIR}{name}"))? != *data {
            let _ = fs::remove_file(&out);
            return Err("vérification du pak généré échouée".into());
        }
    }
    Ok(out)
}

fn main() {
    let game = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if !game.join("Project/Content/Paks/pakchunk0-WindowsNoEditor.pak").is_file() {
        eprintln!("Dossier du jeu introuvable. Usage : ghost-dlc-fix <dossier SMT5V>");
        std::process::exit(2);
    }
    match run(&game) {
        Ok(out) => println!("Patch installé : {}\nPour l'enlever, supprimez ce fichier.", out.display()),
        Err(e) => {
            eprintln!("Erreur : {e}");
            std::process::exit(1);
        }
    }
}
