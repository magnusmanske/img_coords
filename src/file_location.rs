use base64::{Engine, engine::general_purpose};
use exif::{Exif, In, Tag, Value};
use geojson::Feature;
use image::DynamicImage;
use image::io::Reader as ImageReader;
use kml::Kml;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    fs::File,
    io::{BufReader, Cursor},
    path::Path,
};

/// A latitude in degrees, guaranteed to be finite and within `[-90, 90]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Latitude(f64);

impl Latitude {
    pub fn new(value: f64) -> Option<Self> {
        (-90.0..=90.0).contains(&value).then_some(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for Latitude {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for Latitude {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| serde::de::Error::custom("latitude out of range"))
    }
}

/// A longitude in degrees, guaranteed to be finite and within `[-180, 180]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Longitude(f64);

impl Longitude {
    pub fn new(value: f64) -> Option<Self> {
        (-180.0..=180.0).contains(&value).then_some(Self(value))
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Serialize for Longitude {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.0)
    }
}

impl<'de> Deserialize<'de> for Longitude {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::new(value).ok_or_else(|| serde::de::Error::custom("longitude out of range"))
    }
}

/// Image extensions we attempt to read. Includes TIFF/ISOBMFF-based RAW formats,
/// whose EXIF blocks `kamadak-exif` can usually read.
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "tif", "tiff", "webp", "bmp", "heic", "heif", "avif", "dng",
    "cr2", "cr3", "nef", "nrw", "arw", "srf", "sr2", "rw2", "orf", "raf", "pef", "srw", "dcr",
    "kdc", "rwl", "raw",
];

/// Whether the extension is one we try to read coordinates from.
pub fn is_supported_image(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    SUPPORTED_EXTENSIONS
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
}

/// Longest edge of a generated thumbnail, in pixels.
const THUMBNAIL_MAX_EDGE: u32 = 256;
/// JPEG quality of a generated thumbnail.
const THUMBNAIL_QUALITY: u8 = 80;
/// Skip files above this size before attempting to decode them.
const MAX_DECODE_BYTES: u64 = 200 * 1024 * 1024;
/// Refuse to decode images larger than this many pixels (decompression bombs).
const MAX_IMAGE_PIXELS: u64 = 100_000_000;
/// Refuse to embed thumbnails larger than this many bytes.
const MAX_THUMBNAIL_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileLocation {
    pub file: String,
    pub latitude: Latitude,
    pub longitude: Longitude,
    pub altitude: Option<f64>, // negative when below sea level (from GPSAltitudeRef)
    pub direction: Option<f64>, // GPSImgDirection
    pub direction_ref: Option<DirectionRef>, // true or magnetic north
    pub thumbnail: Option<String>,
    pub timestamp: Option<String>,
    /// UTC offset of `timestamp`, e.g. `+01:00`, when known.
    pub utc_offset: Option<String>,
}

/// Borrowed GeoJSON view of a [`FileLocation`], serialized without an intermediate
/// `serde_json::Value` tree.
#[derive(Serialize)]
struct GeoJsonFeature<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    geometry: GeoJsonGeometry,
    properties: GeoJsonProperties<'a>,
}

#[derive(Serialize)]
struct GeoJsonGeometry {
    #[serde(rename = "type")]
    kind: &'static str,
    coordinates: Vec<f64>,
}

#[derive(Serialize)]
struct GeoJsonProperties<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    direction_ref: Option<DirectionRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    utc_offset: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail: Option<&'a str>,
}

/// Reference frame for `GPSImgDirection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectionRef {
    True,
    Magnetic,
}

impl DirectionRef {
    fn from_char(value: char) -> Option<Self> {
        match value.to_ascii_uppercase() {
            'T' => Some(Self::True),
            'M' => Some(Self::Magnetic),
            _ => None,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "true" => Some(Self::True),
            "magnetic" => Some(Self::Magnetic),
            _ => None,
        }
    }
}

/// Why a candidate file did not produce a location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SkipReason {
    OpenFailed,
    NoExif,
    MissingGpsReference,
    MissingCoordinates,
    InvalidCoordinates,
}

impl SkipReason {
    pub fn description(self) -> &'static str {
        match self {
            Self::OpenFailed => "cannot open file",
            Self::NoExif => "no readable EXIF data",
            Self::MissingGpsReference => "missing GPS hemisphere reference",
            Self::MissingCoordinates => "no GPS coordinates",
            Self::InvalidCoordinates => "invalid GPS coordinates",
        }
    }
}

impl FileLocation {
    pub fn from_file(file_name: &str) -> Result<Self, SkipReason> {
        let file = std::fs::File::open(file_name).map_err(|_| SkipReason::OpenFailed)?;
        let mut bufreader = std::io::BufReader::new(&file);
        let exif = exif::Reader::new()
            .read_from_container(&mut bufreader)
            .map_err(|_| SkipReason::NoExif)?;
        Self::from_exif(file_name, &exif)
    }

    pub fn as_kml(&self) -> String {
        let mut other = String::new();
        if let Some(when) = self.timestamp_iso() {
            other += &format!("<TimeStamp><when>{when}</when></TimeStamp>");
        }
        // Omit the altitude component when unknown so viewers clamp the point to the
        // ground instead of pinning it to sea level.
        let (altitude_mode, altitude) = match self.altitude {
            Some(altitude) => ("absolute", format!(",{altitude}")),
            None => ("clampToGround", String::new()),
        };
        format!(
            "<Placemark><name>{}</name><Point><altitudeMode>{altitude_mode}</altitudeMode><coordinates>{},{}{altitude}</coordinates></Point>{other}</Placemark>",
            self.name_xml_escaped(),
            self.longitude.get(),
            self.latitude.get(),
        )
    }

    pub fn as_geojson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.geojson_feature())
    }

    /// CSV row (without the trailing newline).
    pub fn as_csv(&self) -> String {
        let fields = [
            self.file.clone(),
            self.latitude.get().to_string(),
            self.longitude.get().to_string(),
            self.altitude.map(|v| v.to_string()).unwrap_or_default(),
            self.direction.map(|v| v.to_string()).unwrap_or_default(),
            self.timestamp.clone().unwrap_or_default(),
        ];
        fields
            .iter()
            .map(|field| csv_escape(field))
            .collect::<Vec<_>>()
            .join(",")
    }

    /// GPX waypoint.
    pub fn as_gpx(&self) -> String {
        let mut body = format!(
            r#"<wpt lat="{}" lon="{}">"#,
            self.latitude.get(),
            self.longitude.get()
        );
        body += &format!("<name>{}</name>", self.name_xml_escaped());
        if let Some(altitude) = self.altitude {
            body += &format!("<ele>{altitude}</ele>");
        }
        if let Some(when) = self.timestamp_iso() {
            body += &format!("<time>{when}</time>");
        }
        body += "</wpt>";
        body
    }

    fn geojson_feature(&self) -> GeoJsonFeature<'_> {
        // Altitude is the optional third coordinate element, per the GeoJSON spec.
        let mut coordinates = vec![self.longitude.get(), self.latitude.get()];
        if let Some(altitude) = self.altitude {
            coordinates.push(altitude);
        }
        GeoJsonFeature {
            kind: "Feature",
            geometry: GeoJsonGeometry {
                kind: "Point",
                coordinates,
            },
            properties: GeoJsonProperties {
                name: &self.file,
                direction: self.direction,
                direction_ref: self.direction_ref,
                timestamp: self.timestamp.as_deref(),
                utc_offset: self.utc_offset.as_deref(),
                thumbnail: self.thumbnail.as_deref(),
            },
        }
    }

    pub fn generate_missing_thumbnail(&mut self) {
        if self.thumbnail.is_none() {
            self.thumbnail = self.get_thumbnail_base64();
        }
    }

    fn get_thumbnail_base64(&self) -> Option<String> {
        self.decode_thumbnail()
            .map(|bytes| general_purpose::STANDARD_NO_PAD.encode(bytes))
    }

    /// Decodes the image, bounds its dimensions, and re-encodes it as a small JPEG.
    /// Returns `None` for unsupported formats and for files that fail the size guards.
    fn decode_thumbnail(&self) -> Option<Vec<u8>> {
        let metadata = std::fs::metadata(&self.file).ok()?;
        if metadata.len() > MAX_DECODE_BYTES {
            return None;
        }
        // Read just the dimensions first so a decompression bomb is never decoded.
        let (width, height) = ImageReader::new(BufReader::new(File::open(&self.file).ok()?))
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok()?;
        if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
            return None;
        }
        let image = ImageReader::new(BufReader::new(File::open(&self.file).ok()?))
            .with_guessed_format()
            .ok()?
            .decode()
            .ok()?;
        let image = apply_orientation(image, self.exif_orientation());
        let image = if image.width() > THUMBNAIL_MAX_EDGE || image.height() > THUMBNAIL_MAX_EDGE {
            image.thumbnail(THUMBNAIL_MAX_EDGE, THUMBNAIL_MAX_EDGE)
        } else {
            image
        };
        let mut buffer = Cursor::new(Vec::new());
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffer, THUMBNAIL_QUALITY);
        encoder.encode_image(&image).ok()?;
        let bytes = buffer.into_inner();
        (bytes.len() <= MAX_THUMBNAIL_BYTES).then_some(bytes)
    }

    /// Reads the EXIF orientation (1-8), defaulting to 1 (upright).
    fn exif_orientation(&self) -> u16 {
        let Some(exif) = File::open(&self.file).ok().and_then(|file| {
            exif::Reader::new()
                .read_from_container(&mut BufReader::new(&file))
                .ok()
        }) else {
            return 1;
        };
        match exif
            .get_field(Tag::Orientation, In::PRIMARY)
            .map(|f| &f.value)
        {
            Some(Value::Short(values)) => values.first().copied().unwrap_or(1),
            _ => 1,
        }
    }

    pub fn from_kml_element(element: &Kml) -> Option<Self> {
        if let Kml::Placemark(pm) = element
            && let (Some(name), Some(kml::types::Geometry::Point(point))) = (&pm.name, &pm.geometry)
        {
            let (timestamp, utc_offset) = Self::timestamp_from_kml(&pm.children);
            return Some(Self {
                file: name.to_owned(),
                latitude: Latitude::new(point.coord.y)?,
                longitude: Longitude::new(point.coord.x)?,
                altitude: point.coord.z.filter(|value| value.is_finite()),
                direction: None, // Not encoded in KML
                direction_ref: None,
                thumbnail: None,
                timestamp,
                utc_offset,
            });
        }
        None
    }

    /// Reads back the `<TimeStamp><when>` written by `as_kml`, returning the EXIF-style
    /// local timestamp and its UTC offset (if the value carried one).
    fn timestamp_from_kml(children: &[kml::types::Element]) -> (Option<String>, Option<String>) {
        let Some(when) = children
            .iter()
            .find(|e| e.name == "TimeStamp")
            .and_then(|e| e.children.iter().find(|e| e.name == "when"))
            .and_then(|e| e.content.as_deref())
        else {
            return (None, None);
        };
        if let Ok(datetime) = chrono::DateTime::parse_from_rfc3339(when) {
            let timestamp = datetime
                .naive_local()
                .format("%Y:%m:%d %H:%M:%S")
                .to_string();
            return (Some(timestamp), Some(datetime.offset().to_string()));
        }
        match chrono::NaiveDateTime::parse_from_str(when, "%Y-%m-%dT%H:%M:%S") {
            Ok(datetime) => (Some(datetime.format("%Y:%m:%d %H:%M:%S").to_string()), None),
            Err(_) => (None, None),
        }
    }

    /// Builds a location from a typed GeoJSON feature.
    pub fn from_feature(feature: &Feature) -> Option<Self> {
        let point = match &feature.geometry.as_ref()?.value {
            geojson::Value::Point(point) => point,
            _ => return None,
        };
        let properties = feature.properties.as_ref();
        let property = |key: &str| properties.and_then(|p| p.get(key));
        Some(Self {
            file: property("name")?.as_str()?.to_string(),
            latitude: Latitude::new(*point.get(1)?)?,
            longitude: Longitude::new(*point.first()?)?,
            // Altitude is the third coordinate per the GeoJSON spec; also accept the
            // legacy `properties.altitude` emitted by older versions of this tool.
            altitude: point
                .get(2)
                .copied()
                .or_else(|| property("altitude").and_then(serde_json::Value::as_f64)),
            direction: property("direction").and_then(serde_json::Value::as_f64),
            direction_ref: property("direction_ref")
                .and_then(serde_json::Value::as_str)
                .and_then(DirectionRef::parse),
            thumbnail: property("thumbnail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            timestamp: property("timestamp")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            utc_offset: property("utc_offset")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
        })
    }

    /// Convenience wrapper around [`Self::from_feature`] for callers holding a JSON value.
    pub fn from_geojson_feature(v: &serde_json::Value) -> Option<Self> {
        let feature: Feature = serde_json::from_value(v.clone()).ok()?;
        Self::from_feature(&feature)
    }

    fn name_xml_escaped(&self) -> String {
        xml_escape(&self.file)
    }

    pub fn timestamp_parsed(&self) -> Option<chrono::NaiveDateTime> {
        self.timestamp
            .as_ref()
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok())
    }

    fn from_exif(file: &str, exif: &Exif) -> Result<Self, SkipReason> {
        let lat_ref_field = exif
            .get_field(Tag::GPSLatitudeRef, In::PRIMARY)
            .ok_or(SkipReason::MissingGpsReference)?;
        let lat_ref =
            Self::letter_from_value(&lat_ref_field.value).ok_or(SkipReason::InvalidCoordinates)?;
        let lon_ref_field = exif
            .get_field(Tag::GPSLongitudeRef, In::PRIMARY)
            .ok_or(SkipReason::MissingGpsReference)?;
        let lon_ref =
            Self::letter_from_value(&lon_ref_field.value).ok_or(SkipReason::InvalidCoordinates)?;
        let lat_field = exif
            .get_field(Tag::GPSLatitude, In::PRIMARY)
            .ok_or(SkipReason::MissingCoordinates)?;
        let lon_field = exif
            .get_field(Tag::GPSLongitude, In::PRIMARY)
            .ok_or(SkipReason::MissingCoordinates)?;
        let latitude = Self::lat_from_value(&lat_field.value, lat_ref)
            .ok_or(SkipReason::InvalidCoordinates)?;
        let longitude = Self::lon_from_value(&lon_field.value, lon_ref)
            .ok_or(SkipReason::InvalidCoordinates)?;
        let (timestamp, utc_offset) = Self::timestamp_from_exif(exif);
        Ok(Self {
            file: file.to_string(),
            latitude,
            longitude,
            altitude: Self::altitude_from_exif(exif),
            direction: exif
                .get_field(Tag::GPSImgDirection, In::PRIMARY)
                .and_then(|f| Self::f64_from_value(&f.value))
                .filter(|value| value.is_finite()),
            direction_ref: exif
                .get_field(Tag::GPSImgDirectionRef, In::PRIMARY)
                .and_then(|f| Self::letter_from_value(&f.value))
                .and_then(DirectionRef::from_char),
            thumbnail: None,
            timestamp,
            utc_offset,
        })
    }

    /// Returns the local timestamp plus its UTC offset. Falls back to the UTC GPS date and
    /// time when `DateTimeOriginal` is absent.
    fn timestamp_from_exif(exif: &Exif) -> (Option<String>, Option<String>) {
        let timestamp = Self::string_from_value(exif.get_field(Tag::DateTimeOriginal, In::PRIMARY));
        let offset = Self::string_from_value(exif.get_field(Tag::OffsetTimeOriginal, In::PRIMARY))
            .map(|offset| offset.trim().to_string())
            .filter(|offset| Self::valid_utc_offset(offset));
        if timestamp.is_some() {
            return (timestamp, offset);
        }
        let date = Self::string_from_value(exif.get_field(Tag::GPSDateStamp, In::PRIMARY));
        let time = exif
            .get_field(Tag::GPSTimeStamp, In::PRIMARY)
            .and_then(|field| Self::hms_from_value(&field.value));
        match (date, time) {
            (Some(date), Some((hour, minute, second))) => (
                Some(format!("{date} {hour:02}:{minute:02}:{second:02}")),
                Some("+00:00".to_string()),
            ),
            _ => (None, None),
        }
    }

    fn valid_utc_offset(offset: &str) -> bool {
        let bytes = offset.as_bytes();
        bytes.len() == 6
            && matches!(bytes[0], b'+' | b'-')
            && bytes[3] == b':'
            && bytes[1..3].iter().all(u8::is_ascii_digit)
            && bytes[4..6].iter().all(u8::is_ascii_digit)
    }

    fn hms_from_value(value: &Value) -> Option<(u8, u8, u8)> {
        let Value::Rational(parts) = value else {
            return None;
        };
        let hour = parts.first()?.to_f64();
        let minute = parts.get(1)?.to_f64();
        let second = parts.get(2)?.to_f64();
        if !(0.0..24.0).contains(&hour)
            || !(0.0..60.0).contains(&minute)
            || !(0.0..60.0).contains(&second)
        {
            return None;
        }
        Some((hour as u8, minute as u8, second as u8))
    }

    /// ISO 8601 representation of the timestamp, including the UTC offset when known.
    fn timestamp_iso(&self) -> Option<String> {
        let timestamp = self.timestamp_parsed()?;
        let base = timestamp.format("%Y-%m-%dT%H:%M:%S").to_string();
        Some(match &self.utc_offset {
            Some(offset) => format!("{base}{offset}"),
            None => base,
        })
    }

    /// Reads GPS altitude, applying the sign from `GPSAltitudeRef` (0 = above, 1 = below
    /// sea level). The tag is absent on many images and defaults to above sea level.
    fn altitude_from_exif(exif: &Exif) -> Option<f64> {
        let altitude = Self::f64_from_value(&exif.get_field(Tag::GPSAltitude, In::PRIMARY)?.value)?;
        let below_sea_level = exif
            .get_field(Tag::GPSAltitudeRef, In::PRIMARY)
            .is_some_and(|f| matches!(&f.value, Value::Byte(v) if v.first() == Some(&1)));
        Some(if below_sea_level { -altitude } else { altitude }).filter(|value| value.is_finite())
    }

    fn string_from_value(f: Option<&exif::Field>) -> Option<String> {
        if let Some(f) = f
            && let Value::Ascii(vs) = &f.value
            && let Some(s) = vs.first()
            && let Ok(ts) = std::str::from_utf8(s)
        {
            return Some(ts.to_string());
        }
        None
    }

    fn letter_from_value(v: &Value) -> Option<char> {
        match v {
            Value::Ascii(chars) => Some(*(chars.first()?.first()?) as char),
            _ => None,
        }
    }

    fn f64_from_value(v: &Value) -> Option<f64> {
        match v {
            Value::Rational(r) => Some(r.first()?.to_f64()),
            _ => None,
        }
    }

    fn lat_from_value(v: &Value, r: char) -> Option<Latitude> {
        Latitude::new(Self::coord_from_value(v, r, 'N', 'S')?)
    }

    fn lon_from_value(v: &Value, r: char) -> Option<Longitude> {
        Longitude::new(Self::coord_from_value(v, r, 'E', 'W')?)
    }

    /// Converts EXIF degrees/minutes/seconds plus a hemisphere reference to decimal
    /// degrees. Returns `None` for malformed values or an unexpected reference.
    fn coord_from_value(v: &Value, reference: char, positive: char, negative: char) -> Option<f64> {
        let Value::Rational(parts) = v else {
            return None;
        };
        let degrees = parts.first()?.to_f64();
        let minutes = parts.get(1)?.to_f64();
        let seconds = parts.get(2)?.to_f64();
        let mut value = degrees + minutes / 60.0 + seconds / 3600.0;
        match reference.to_ascii_uppercase() {
            c if c == positive => {}
            c if c == negative => value = -value,
            _ => return None,
        }
        value.is_finite().then_some(value)
    }
}

/// Applies an EXIF orientation (1-8) to an image.
fn apply_orientation(image: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
}

/// Escapes text for XML 1.0, replacing characters that are not allowed in XML.
fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\t' | '\n' | '\r' => escaped.push(character),
            c if (c as u32) < 0x20 => escaped.push('\u{FFFD}'),
            c => escaped.push(c),
        }
    }
    escaped
}

/// Quotes a CSV field if it contains a comma, quote, or line break.
fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn lat(value: f64) -> Latitude {
        Latitude::new(value).unwrap()
    }

    fn lon(value: f64) -> Longitude {
        Longitude::new(value).unwrap()
    }

    #[test]
    fn test_coordinate_ranges() {
        assert!(Latitude::new(90.0).is_some());
        assert!(Latitude::new(-90.0).is_some());
        assert!(Latitude::new(90.1).is_none());
        assert!(Latitude::new(f64::NAN).is_none());
        assert!(Longitude::new(180.0).is_some());
        assert!(Longitude::new(-180.0).is_some());
        assert!(Longitude::new(180.1).is_none());
        assert!(Longitude::new(f64::INFINITY).is_none());
    }

    #[test]
    fn test_from_file() {
        let fl = FileLocation::from_file("test_files/sunrise.jpg").unwrap();
        println!("{fl:?}");
        assert_eq!(fl.file, "test_files/sunrise.jpg");
        assert_eq!(fl.latitude.get(), 45.50566666666667);
        assert_eq!(fl.longitude.get(), 12.341161111111111);
        assert_eq!(fl.altitude, Some(46.79));
        assert_eq!(fl.direction, Some(11.0));
        assert_eq!(fl.thumbnail, None);
        assert_eq!(fl.timestamp, Some("2025:03:06 05:41:42".to_string()));
    }

    fn exif_with_fields(fields: Vec<exif::Field>) -> Exif {
        let mut writer = exif::experimental::Writer::new();
        for field in &fields {
            writer.push_field(field);
        }
        let mut buf = Cursor::new(Vec::new());
        writer.write(&mut buf, false).unwrap();
        exif::Reader::new().read_raw(buf.into_inner()).unwrap()
    }

    fn primary(tag: Tag, value: Value) -> exif::Field {
        exif::Field {
            tag,
            ifd_num: In::PRIMARY,
            value,
        }
    }

    fn rationals(values: &[(u32, u32)]) -> Value {
        Value::Rational(
            values
                .iter()
                .map(|&(num, denom)| exif::Rational { num, denom })
                .collect(),
        )
    }

    /// Many images have GPS coordinates but no altitude or camera direction. Those
    /// optional tags must not cause the whole image to be dropped.
    #[test]
    fn test_from_exif_without_altitude_or_direction() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (30, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
            primary(
                Tag::DateTimeOriginal,
                Value::Ascii(vec![b"2025:03:06 05:41:42".to_vec()]),
            ),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif)
            .expect("missing altitude/direction must not drop the image");
        assert_eq!(fl.altitude, None);
        assert_eq!(fl.direction, None);
        assert!((fl.latitude.get() - 45.5).abs() < 1e-9);
        assert!((fl.longitude.get() - 12.0).abs() < 1e-9);
        assert_eq!(fl.timestamp.as_deref(), Some("2025:03:06 05:41:42"));
    }

    #[test]
    fn test_direction_reference() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSImgDirection, rationals(&[(90, 1)])),
            primary(Tag::GPSImgDirectionRef, Value::Ascii(vec![b"M".to_vec()])),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif).unwrap();
        assert_eq!(fl.direction, Some(90.0));
        assert_eq!(fl.direction_ref, Some(DirectionRef::Magnetic));
    }

    #[test]
    fn test_offset_time_original() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
            primary(
                Tag::DateTimeOriginal,
                Value::Ascii(vec![b"2025:03:06 05:41:42".to_vec()]),
            ),
            primary(
                Tag::OffsetTimeOriginal,
                Value::Ascii(vec![b"+02:00".to_vec()]),
            ),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif).unwrap();
        assert_eq!(fl.utc_offset.as_deref(), Some("+02:00"));
    }

    #[test]
    fn test_gps_timestamp_fallback_is_utc() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
            primary(
                Tag::GPSDateStamp,
                Value::Ascii(vec![b"2025:03:06".to_vec()]),
            ),
            primary(Tag::GPSTimeStamp, rationals(&[(5, 1), (41, 1), (42, 1)])),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif).unwrap();
        assert_eq!(fl.timestamp.as_deref(), Some("2025:03:06 05:41:42"));
        assert_eq!(fl.utc_offset.as_deref(), Some("+00:00"));
    }

    #[test]
    fn test_south_west_coordinates() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"S".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"W".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (30, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif).unwrap();
        assert!((fl.latitude.get() + 45.5).abs() < 1e-9);
        assert!((fl.longitude.get() + 12.0).abs() < 1e-9);
    }

    /// `GPSAltitudeRef == 1` means below sea level, so the altitude must be negated.
    #[test]
    fn test_negative_altitude() {
        let exif = exif_with_fields(vec![
            primary(Tag::GPSLatitudeRef, Value::Ascii(vec![b"N".to_vec()])),
            primary(Tag::GPSLongitudeRef, Value::Ascii(vec![b"E".to_vec()])),
            primary(Tag::GPSLatitude, rationals(&[(45, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSLongitude, rationals(&[(12, 1), (0, 1), (0, 1)])),
            primary(Tag::GPSAltitude, rationals(&[(100, 5)])), // 20 m
            primary(Tag::GPSAltitudeRef, Value::Byte(vec![1])),
        ]);
        let fl = FileLocation::from_exif("x.jpg", &exif).unwrap();
        assert_eq!(fl.altitude, Some(-20.0));
    }

    #[test]
    fn test_from_geojson_feature() {
        let v = serde_json::json!({
            "type": "Feature",
            "geometry": {
                "type": "Point",
                "coordinates": [12.345, 45.6789]
            },
            "properties": {
                "name": "test_files/sunrise.jpg",
                "altitude": 46.79,
                "direction": 11.0,
                "timestamp": "2025:03:06 05:41:42",
                "thumbnail": "base64"
            }
        });
        let fl = FileLocation::from_geojson_feature(&v).unwrap();
        assert_eq!(fl.file, "test_files/sunrise.jpg");
        assert_eq!(fl.latitude.get(), 45.6789);
        assert_eq!(fl.longitude.get(), 12.345);
        assert_eq!(fl.altitude, Some(46.79));
        assert_eq!(fl.direction, Some(11.0));
        assert_eq!(fl.thumbnail, Some("base64".to_string()));
        assert_eq!(fl.timestamp, Some("2025:03:06 05:41:42".to_string()));
    }

    #[test]
    fn test_from_geojson_feature_altitude_in_coordinates() {
        let feature: Feature = serde_json::from_value(serde_json::json!({
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [12.345, 45.6789, 123.5] },
            "properties": { "name": "x.jpg" }
        }))
        .unwrap();
        let fl = FileLocation::from_feature(&feature).unwrap();
        assert_eq!(fl.altitude, Some(123.5));
    }

    #[test]
    fn test_as_csv_escapes() {
        let fl = FileLocation {
            file: "a,b\"c.jpg".to_string(),
            latitude: lat(1.0),
            longitude: lon(2.0),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: None,
            utc_offset: None,
        };
        assert_eq!(fl.as_csv(), "\"a,b\"\"c.jpg\",1,2,,,");
    }

    #[test]
    fn test_as_gpx() {
        let fl = FileLocation {
            file: "a&b.jpg".to_string(),
            latitude: lat(1.0),
            longitude: lon(2.0),
            altitude: Some(3.0),
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: None,
        };
        let gpx = fl.as_gpx();
        assert!(gpx.contains(r#"lat="1" lon="2""#), "{gpx}");
        assert!(gpx.contains("<name>a&amp;b.jpg</name>"), "{gpx}");
        assert!(gpx.contains("<ele>3</ele>"), "{gpx}");
        assert!(gpx.contains("<time>2025-03-06T05:41:42</time>"), "{gpx}");
    }

    #[test]
    fn test_apply_orientation_swaps_dimensions() {
        let image = DynamicImage::new_rgb8(2, 3);
        assert_eq!(apply_orientation(image.clone(), 3).width(), 2);
        assert_eq!(apply_orientation(image.clone(), 6).width(), 3);
        assert_eq!(apply_orientation(image.clone(), 6).height(), 2);
        assert_eq!(apply_orientation(image, 8).width(), 3);
    }

    #[test]
    fn test_raw_extensions_supported() {
        for extension in ["dng", "CR2", "nef", "arw", "rw2", "orf", "raf", "cr3"] {
            assert!(
                is_supported_image(Path::new(&format!("a.{extension}"))),
                "{extension}"
            );
        }
        assert!(!is_supported_image(Path::new("a.txt")));
    }

    #[test]
    fn test_from_kml_element() {
        let kml = Kml::from_str(
            r#"
			<Placemark>
				<name>test_files/sunrise.jpg</name>
				<Point>
					<coordinates>12.345,45.6789,46.79</coordinates>
				</Point>
			</Placemark>
			"#,
        )
        .unwrap();
        let fl = FileLocation::from_kml_element(&kml).unwrap();
        assert_eq!(fl.file, "test_files/sunrise.jpg");
        assert_eq!(fl.latitude.get(), 45.6789);
        assert_eq!(fl.longitude.get(), 12.345);
        assert_eq!(fl.altitude, Some(46.79));
        assert_eq!(fl.direction, None);
        assert_eq!(fl.thumbnail, None);
        assert_eq!(fl.timestamp, None);
    }

    #[test]
    fn test_as_kml() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: Some(11.0),
            direction_ref: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: None,
        };
        let kml = fl.as_kml();
        assert_eq!(
            kml,
            "<Placemark><name>test_files/sunrise.jpg</name><Point><altitudeMode>absolute</altitudeMode><coordinates>12.345,45.6789,46.79</coordinates></Point><TimeStamp><when>2025-03-06T05:41:42</when></TimeStamp></Placemark>"
        );
    }

    #[test]
    fn test_kml_without_altitude_clamps_to_ground() {
        let fl = FileLocation {
            file: "a.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: None,
            utc_offset: None,
        };
        let kml = fl.as_kml();
        assert!(
            kml.contains(
                "<altitudeMode>clampToGround</altitudeMode><coordinates>12.345,45.6789</coordinates>"
            ),
            "{kml}"
        );
        let back = FileLocation::from_kml_element(&Kml::from_str(&kml).unwrap()).unwrap();
        assert_eq!(back.altitude, None);
    }

    #[test]
    fn test_kml_timestamp_roundtrip() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: None,
        };
        let kml = Kml::from_str(&fl.as_kml()).unwrap();
        let back = FileLocation::from_kml_element(&kml).unwrap();
        assert_eq!(back.timestamp, fl.timestamp);
    }

    #[test]
    fn test_kml_timestamp_with_offset_roundtrip() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: Some("+01:00".to_string()),
        };
        let text = fl.as_kml();
        assert!(
            text.contains("<when>2025-03-06T05:41:42+01:00</when>"),
            "{text}"
        );
        let back = FileLocation::from_kml_element(&Kml::from_str(&text).unwrap()).unwrap();
        assert_eq!(back.timestamp, fl.timestamp);
        assert_eq!(back.utc_offset, fl.utc_offset);
    }

    #[test]
    fn test_as_geojson() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: Some(11.0),
            direction_ref: None,
            thumbnail: Some("base64".to_string()),
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: None,
        };
        let geojson = fl.as_geojson().unwrap();
        let expected = r#"{"type":"Feature","geometry":{"type":"Point","coordinates":[12.345,45.6789,46.79]},"properties":{"name":"test_files/sunrise.jpg","direction":11.0,"timestamp":"2025:03:06 05:41:42","thumbnail":"base64"}}"#;
        assert_eq!(geojson, expected);
    }

    /// `as_geojson` -> `from_geojson_feature` must reproduce every field. This guards
    /// the update workflow: emit GeoJSON, then re-load it with `--update`.
    #[test]
    fn test_geojson_roundtrip_full() {
        let fl = FileLocation {
            file: "a/b c.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: Some(11.0),
            direction_ref: Some(DirectionRef::Magnetic),
            thumbnail: Some("base64data".to_string()),
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: Some("+01:00".to_string()),
        };
        let v: serde_json::Value = serde_json::from_str(&fl.as_geojson().unwrap()).unwrap();
        let back = FileLocation::from_geojson_feature(&v).unwrap();
        assert_eq!(back.file, fl.file);
        assert_eq!(back.latitude, fl.latitude);
        assert_eq!(back.longitude, fl.longitude);
        assert_eq!(back.utc_offset, fl.utc_offset);
        assert_eq!(back.altitude, fl.altitude);
        assert_eq!(back.direction, fl.direction);
        assert_eq!(back.thumbnail, fl.thumbnail);
        assert_eq!(back.timestamp, fl.timestamp);
    }

    /// Same round-trip but for a location with no altitude/direction/thumbnail/timestamp
    /// (common for images with GPS coords but no other GPS tags). `as_geojson` omits the
    /// missing keys, so `from_geojson_feature` must tolerate their absence.
    #[test]
    fn test_geojson_roundtrip_minimal() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: None,
            utc_offset: None,
        };
        let v: serde_json::Value = serde_json::from_str(&fl.as_geojson().unwrap()).unwrap();
        let back = FileLocation::from_geojson_feature(&v)
            .expect("a location without altitude/direction must still round-trip");
        assert_eq!(back.file, fl.file);
        assert_eq!(back.latitude, fl.latitude);
        assert_eq!(back.longitude, fl.longitude);
        assert_eq!(back.altitude, None);
        assert_eq!(back.direction, None);
        assert_eq!(back.thumbnail, None);
        assert_eq!(back.timestamp, None);
    }

    #[test]
    fn test_from_geojson_feature_missing_name() {
        let v = serde_json::json!({
            "type": "Feature",
            "geometry": { "type": "Point", "coordinates": [12.345, 45.6789] },
            "properties": {}
        });
        assert!(FileLocation::from_geojson_feature(&v).is_none());
    }

    #[test]
    fn test_from_geojson_feature_non_point() {
        let v = serde_json::json!({
            "type": "Feature",
            "geometry": { "type": "LineString", "coordinates": [[0.0, 0.0], [1.0, 1.0]] },
            "properties": { "name": "x.jpg" }
        });
        assert!(FileLocation::from_geojson_feature(&v).is_none());
    }

    #[test]
    fn test_timestamp_parsed() {
        let mut fl = FileLocation {
            file: "x.jpg".to_string(),
            latitude: lat(0.0),
            longitude: lon(0.0),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
            utc_offset: None,
        };
        let parsed = fl.timestamp_parsed().unwrap();
        assert_eq!(parsed.to_string(), "2025-03-06 05:41:42");

        fl.timestamp = Some("not a date".to_string());
        assert!(fl.timestamp_parsed().is_none());

        fl.timestamp = None;
        assert!(fl.timestamp_parsed().is_none());
    }

    /// The file name goes into KML markup verbatim, so it must be XML-escaped or a name
    /// containing `<`/`&`/quotes would produce invalid (or injected) KML.
    #[test]
    fn test_xml_escape_strips_control_characters() {
        let fl = FileLocation {
            file: "a\u{1}b&c.jpg".to_string(),
            latitude: lat(1.0),
            longitude: lon(2.0),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: None,
            utc_offset: None,
        };
        let kml = fl.as_kml();
        assert!(kml.contains("<name>a\u{FFFD}b&amp;c.jpg</name>"), "{kml}");
        assert!(!kml.contains('\u{1}'));
    }

    #[test]
    fn test_as_kml_escapes_name() {
        let fl = FileLocation {
            file: r#"a & b <tag> "q" 'x'.jpg"#.to_string(),
            latitude: lat(1.0),
            longitude: lon(2.0),
            altitude: None,
            direction: None,
            direction_ref: None,
            thumbnail: None,
            timestamp: None,
            utc_offset: None,
        };
        let kml = fl.as_kml();
        assert!(kml.contains("<name>a &amp; b &lt;tag&gt; &quot;q&quot; &apos;x&apos;.jpg</name>"));
        // The escaped name must not reintroduce raw markup characters.
        let name_part = kml
            .split("<name>")
            .nth(1)
            .unwrap()
            .split("</name>")
            .next()
            .unwrap();
        assert!(!name_part.contains('<'));
        assert!(!name_part.contains('>'));
    }
}
