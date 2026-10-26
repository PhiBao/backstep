use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
pub struct RawRecord {
    pub sam_frame: i64,
    pub object_id: i64,
    pub x1: i64,
    pub y1: i64,
    pub x2: i64,
    pub y2: i64,
    pub frame_w: i64,
    pub frame_h: i64,
    pub confidence: Option<f64>,
    pub mask_data: String,
}

/// Parse SAM 3.1 special-token output text into box/mask records.
/// Grammar: `<Nf>id<|box;...|><|mask;...|>,id<|box;...|><|mask;...|>` per frame line.
pub fn parse_sam_text(text: &str) -> Vec<RawRecord> {
    let mut out = Vec::new();
    let frame_re = Regex::new(r"<(\d+)f>").unwrap();
    let rec_re = Regex::new(
        r"(\d+)<\|box;x1=(\d+);y1=(\d+);x2=(\d+);y2=(\d+);w=(\d+);h=(\d+)(?:;c=([0-9.]+))?\|><\|mask;x=0;y=0(?:;c=[0-9.]+)?;data=([^|]+)\|>",
    )
    .unwrap();

    let marks: Vec<(i64, usize, usize)> = frame_re
        .captures_iter(text)
        .filter_map(|c| {
            let m = c.get(0)?;
            Some((c[1].parse::<i64>().ok()?, m.start(), m.end()))
        })
        .collect();

    for (i, (frame, _start, end)) in marks.iter().enumerate() {
        let seg_end = marks.get(i + 1).map(|m| m.1).unwrap_or(text.len());
        let seg = &text[*end..seg_end];
        for c in rec_re.captures_iter(seg) {
            out.push(RawRecord {
                sam_frame: *frame,
                object_id: c[1].parse().unwrap_or(0),
                x1: c[2].parse().unwrap_or(0),
                y1: c[3].parse().unwrap_or(0),
                x2: c[4].parse().unwrap_or(0),
                y2: c[5].parse().unwrap_or(0),
                frame_w: c[6].parse().unwrap_or(0),
                frame_h: c[7].parse().unwrap_or(0),
                confidence: c.get(8).and_then(|m| m.as_str().parse::<f64>().ok()),
                mask_data: c[9].to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_image_line() {
        let text = "text<0f>0<|box;x1=353;y1=382;x2=453;y2=427;w=537;h=561|><|mask;x=0;y=0;data=46,101,~abc|>,1<|box;x1=260;y1=382;x2=340;y2=427;w=537;h=561|><|mask;x=0;y=0;data=46,81,~def|>";
        let recs = parse_sam_text(text);
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].object_id, 0);
        assert_eq!(recs[0].x1, 353);
        assert_eq!(recs[1].sam_frame, 0);
        assert_eq!(recs[1].mask_data, "46,81,~def");
    }

    #[test]
    fn parses_confidence_and_sparse_ids() {
        let text = "text<2f>2<|box;x1=444;y1=625;x2=679;y2=850;w=1800;h=1200;c=0.9609375|><|mask;x=0;y=0;c=0.9609375;data=226,236,!xyz|>";
        let recs = parse_sam_text(text);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].sam_frame, 2);
        assert_eq!(recs[0].object_id, 2);
        assert_eq!(recs[0].confidence, Some(0.9609375));
    }

    #[test]
    fn empty_text_is_no_matches() {
        assert!(parse_sam_text("").is_empty());
        assert!(parse_sam_text("text").is_empty());
    }
}
