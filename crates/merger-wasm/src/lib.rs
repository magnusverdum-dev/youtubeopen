use wasm_bindgen::prelude::*;
use wasm_bindgen::JsValue;
use js_sys::Uint8Array;

extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);
}

fn find_box(data: &[u8], target: &[u8; 4]) -> Option<(usize, usize)> {
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().ok()?) as usize;
        if size < 8 || pos + size > data.len() {
            return None;
        }
        if &data[pos + 4..pos + 8] == target {
            return Some((pos, size));
        }
        pos += size;
    }
    None
}

fn extract_moov_payload(data: &[u8]) -> Result<Vec<u8>, String> {
    let (pos, size) = find_box(data, b"moov").ok_or("no moov in data")?;
    let payload_size = size - 8;
    Ok(data[pos + 8..pos + 8 + payload_size].to_vec())
}

fn extract_trak_from_moov(moov_payload: &[u8]) -> Result<Vec<u8>, String> {
    let (pos, size) = find_box(moov_payload, b"trak").ok_or("no trak in moov")?;
    let payload_size = size - 8;
    Ok(moov_payload[pos + 8..pos + 8 + payload_size].to_vec())
}

fn merge_inits(video_init: &[u8], audio_init: &[u8]) -> Result<Vec<u8>, String> {
    let ftyp_size = find_box(video_init, b"ftyp").map(|(_, sz)| sz).ok_or("no ftyp in video")?;
    let v_moov_payload = extract_moov_payload(video_init)?;
    let a_moov_payload = extract_moov_payload(audio_init)?;
    let a_trak = extract_trak_from_moov(&a_moov_payload)?;
    let mut children = Vec::with_capacity(v_moov_payload.len() + a_trak.len() + 8);
    let mut pos = 0;
    while pos + 8 <= v_moov_payload.len() {
        let sz = u32::from_be_bytes(v_moov_payload[pos..pos + 4].try_into().unwrap()) as usize;
        if sz < 8 || pos + sz > v_moov_payload.len() {
            break;
        }
        children.extend_from_slice(&v_moov_payload[pos..pos + sz]);
        pos += sz;
    }
    children.extend_from_slice(&a_trak);
    let moov_size = 8 + children.len();
    let mut moov = Vec::with_capacity(moov_size);
    moov.extend_from_slice(&(moov_size as u32).to_be_bytes());
    moov.extend_from_slice(b"moov");
    moov.extend_from_slice(&children);
    let mut out = Vec::with_capacity(ftyp_size + moov_size);
    out.extend_from_slice(&video_init[0..ftyp_size]);
    out.extend_from_slice(&moov);
    Ok(out)
}

fn scan_fragments(data: &[u8], tag: &[u8; 4]) -> Vec<(usize, usize)> {
    let mut frags = Vec::new();
    let mut pos = 0;
    while pos + 8 <= data.len() {
        let sz = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        if sz < 8 || pos + sz > data.len() {
            break;
        }
        if &data[pos + 4..pos + 8] == tag {
            frags.push((pos, sz));
        }
        pos += sz;
    }
    frags
}

fn fix_audio_tfhd(data: &mut [u8]) {
    if let Some((tfhd_pos, _)) = find_box(data, b"tfhd") {
        if tfhd_pos + 12 <= data.len() {
            data[tfhd_pos + 8..tfhd_pos + 12].copy_from_slice(&2u32.to_be_bytes());
        }
    }
}

fn interleave(
    merged_init: &[u8],
    v_moofs: &[(usize, usize)],
    v_mdats: &[(usize, usize)],
    a_moofs: &[(usize, usize)],
    a_mdats: &[(usize, usize)],
    v_media: &[u8],
    a_media: &[u8],
    v_init_end: usize,
    a_init_end: usize,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(merged_init.len() + v_media.len() + a_media.len());
    out.extend_from_slice(merged_init);
    let total = v_moofs.len().max(a_moofs.len());
    for i in 0..total {
        if let Some(&(v_pos, v_sz)) = v_moofs.get(i) {
            out.extend_from_slice(&v_media[v_init_end + v_pos..v_init_end + v_pos + v_sz]);
        }
        if let Some(&(v_pos, v_sz)) = v_mdats.get(i) {
            out.extend_from_slice(&v_media[v_init_end + v_pos..v_init_end + v_pos + v_sz]);
        }
        if let Some(&(a_pos, a_sz)) = a_moofs.get(i) {
            let mut a_moof = a_media[a_init_end + a_pos..a_init_end + a_pos + a_sz].to_vec();
            fix_audio_tfhd(&mut a_moof);
            out.extend_from_slice(&a_moof);
        }
        if let Some(&(a_pos, a_sz)) = a_mdats.get(i) {
            out.extend_from_slice(&a_media[a_init_end + a_pos..a_init_end + a_pos + a_sz]);
        }
    }
    out
}

fn do_merge(video: &[u8], audio: &[u8]) -> Result<Vec<u8>, String> {
    if video.len() < 8 || audio.len() < 8 {
        return Err("data too small".into());
    }
    if &video[4..8] != b"ftyp" || &audio[4..8] != b"ftyp" {
        return Err("not fMP4".into());
    }
    let video_ftyp = find_box(video, b"ftyp").ok_or("no ftyp in video")?;
    let video_moov = find_box(video, b"moov").ok_or("no moov in video")?;
    let a_ftyp = find_box(audio, b"ftyp").ok_or("no ftyp in audio")?;
    let a_moov = find_box(audio, b"moov").ok_or("no moov in audio")?;
    let max_video = video_ftyp.1.max(video_moov.1);
    let max_audio = a_ftyp.1.max(a_moov.1);
    if max_video >= video.len() || max_audio >= audio.len() {
        return Err("init segment larger than file".into());
    }
    let v_init = &video[..max_video];
    let a_init = &audio[..max_audio];
    let v_media = &video[max_video..];
    let a_media = &audio[max_audio..];
    let merged_init = merge_inits(v_init, a_init)?;
    let v_moofs = scan_fragments(v_media, b"moof");
    let v_mdats = scan_fragments(v_media, b"mdat");
    let a_moofs = scan_fragments(a_media, b"moof");
    let a_mdats = scan_fragments(a_media, b"mdat");
    let result = interleave(
        &merged_init, &v_moofs, &v_mdats, &a_moofs, &a_mdats,
        v_media, a_media, max_video, max_audio,
    );
    Ok(result)
}

#[wasm_bindgen]
pub fn is_fmp4(data: &[u8]) -> bool {
    data.len() >= 8 && &data[4..8] == b"ftyp"
}

#[wasm_bindgen]
pub fn get_ext_from_mime(mime: &str) -> String {
    if mime.contains("webm") { "webm".into() } else { "mp4".into() }
}

#[wasm_bindgen]
pub fn check_mp4(data: &[u8]) -> String {
    if is_fmp4(data) && find_box(data, b"moov").is_some() {
        "valid_mp4".into()
    } else {
        "invalid".into()
    }
}

#[wasm_bindgen]
pub async fn merge_video_audio(video_bytes: Vec<u8>, audio_bytes: Vec<u8>) -> Result<Uint8Array, String> {
    let result = do_merge(&video_bytes, &audio_bytes)?;
    Ok(Uint8Array::from(result.as_slice()))
}

#[wasm_bindgen(start)]
pub fn init() {
    log("YouTubeOpen WASM merger loaded!");
}
