use base64::{Engine, engine::general_purpose};
use exif::{Exif, In, Tag, Value};
use geojson::Feature;
use kml::Kml;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{
    fs::File,
    io::{BufReader, Cursor},
    path::Path,
};
use thumbnailer::{ThumbnailSize, create_thumbnails};

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

/// MIME type used to decode an image extension we support. Returns `None` for files
/// that are not one of the image formats we try to process.
pub fn image_mime(path: &Path) -> Option<mime::Mime> {
    let ext = path.extension()?.to_str()?;
    let is = |name: &str| ext.eq_ignore_ascii_case(name);
    if is("jpg") || is("jpeg") {
        Some(mime::IMAGE_JPEG)
    } else if is("png") {
        Some(mime::IMAGE_PNG)
    } else if is("gif") {
        Some(mime::IMAGE_GIF)
    } else if is("bmp") {
        Some(mime::IMAGE_BMP)
    } else if is("webp") {
        "image/webp".parse().ok()
    } else if is("tif") || is("tiff") {
        "image/tiff".parse().ok()
    } else if is("heic") {
        "image/heic".parse().ok()
    } else if is("heif") {
        "image/heif".parse().ok()
    } else if is("avif") {
        "image/avif".parse().ok()
    } else {
        None
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileLocation {
    pub file: String,
    pub latitude: Latitude,
    pub longitude: Longitude,
    pub altitude: Option<f64>, // negative when below sea level (from GPSAltitudeRef)
    pub direction: Option<f64>, // GPSImgDirection; magnetic vs. true north is not tracked
    pub thumbnail: Option<String>,
    pub timestamp: Option<String>,
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
    timestamp: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thumbnail: Option<&'a str>,
}

impl FileLocation {
    pub fn from_file(file_name: &str) -> Option<Self> {
        let file = std::fs::File::open(file_name).ok()?;
        let mut bufreader = std::io::BufReader::new(&file);
        let exifreader = exif::Reader::new();
        let exif = exifreader.read_from_container(&mut bufreader).ok()?;
        Self::from_exif(file_name, &exif)
    }

    pub fn as_kml(&self) -> String {
        let mut other = String::new();
        if let Some(when) = self
            .timestamp_parsed()
            .map(|ts| ts.format("%Y-%m-%dT%H:%M:%S").to_string())
        {
            other += &format!("<TimeStamp><when>{when}</when></TimeStamp>");
        }
        format!(
            "<Placemark><name>{}</name><Point><coordinates>{},{},{}</coordinates></Point>{other}</Placemark>",
            self.name_xml_escaped(),
            self.longitude.get(),
            self.latitude.get(),
            self.altitude.unwrap_or(0.0),
        )
    }

    pub fn as_geojson(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(&self.geojson_feature())
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
                timestamp: self.timestamp.as_deref(),
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
        // The thumbnailer picks its decoder from the MIME type, so it must match the
        // actual file format (not always JPEG).
        let mime = image_mime(Path::new(&self.file))?;
        let file = File::open(&self.file).ok()?;
        let reader = BufReader::new(file);
        let thumbnail = create_thumbnails(reader, mime, [ThumbnailSize::Medium]).ok()?;
        let thumbnail = thumbnail.first()?.to_owned();
        let mut buf = Cursor::new(Vec::new());
        thumbnail.write_jpeg(&mut buf, 8).ok()?;
        let vec = buf.into_inner();
        let encoded: String = general_purpose::STANDARD_NO_PAD.encode(vec);
        Some(encoded)
    }

    pub fn from_kml_element(element: &Kml) -> Option<Self> {
        if let Kml::Placemark(pm) = element
            && let (Some(name), Some(kml::types::Geometry::Point(point))) = (&pm.name, &pm.geometry)
        {
            return Some(Self {
                file: name.to_owned(),
                latitude: Latitude::new(point.coord.y)?,
                longitude: Longitude::new(point.coord.x)?,
                altitude: point.coord.z.filter(|value| value.is_finite()),
                direction: None, // Not encoded in KML
                thumbnail: None,
                timestamp: Self::timestamp_from_kml(&pm.children),
            });
        }
        None
    }

    /// Reads back the `<TimeStamp><when>` written by `as_kml`, converting the ISO 8601
    /// value to the EXIF-style timestamp format used internally.
    fn timestamp_from_kml(children: &[kml::types::Element]) -> Option<String> {
        let when = children
            .iter()
            .find(|e| e.name == "TimeStamp")?
            .children
            .iter()
            .find(|e| e.name == "when")?
            .content
            .as_deref()?;
        chrono::NaiveDateTime::parse_from_str(when, "%Y-%m-%dT%H:%M:%S")
            .ok()
            .map(|dt| dt.format("%Y:%m:%d %H:%M:%S").to_string())
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
            thumbnail: property("thumbnail")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            timestamp: property("timestamp")
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
        self.file
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&apos;")
    }

    pub fn timestamp_parsed(&self) -> Option<chrono::NaiveDateTime> {
        self.timestamp
            .as_ref()
            .and_then(|s| chrono::NaiveDateTime::parse_from_str(s, "%Y:%m:%d %H:%M:%S").ok())
    }

    fn from_exif(file: &str, exif: &Exif) -> Option<Self> {
        let lat_ref =
            Self::letter_from_value(&exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY)?.value)?;
        let lon_ref =
            Self::letter_from_value(&exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY)?.value)?;
        let timestamp = Self::string_from_value(exif.get_field(Tag::DateTimeOriginal, In::PRIMARY));
        Some(Self {
            file: file.to_string(),
            latitude: Self::lat_from_value(
                &exif.get_field(Tag::GPSLatitude, In::PRIMARY)?.value,
                lat_ref,
            )?,
            longitude: Self::lon_from_value(
                &exif.get_field(Tag::GPSLongitude, In::PRIMARY)?.value,
                lon_ref,
            )?,
            altitude: Self::altitude_from_exif(exif),
            direction: exif
                .get_field(Tag::GPSImgDirection, In::PRIMARY)
                .and_then(|f| Self::f64_from_value(&f.value))
                .filter(|value| value.is_finite()),
            thumbnail: None,
            timestamp,
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
    fn test_image_mime_supports_phone_formats() {
        for (path, expected) in [
            ("a.JPG", "image/jpeg"),
            ("a.jpeg", "image/jpeg"),
            ("a.HEIC", "image/heic"),
            ("a.heif", "image/heif"),
            ("a.webp", "image/webp"),
            ("a.avif", "image/avif"),
            ("a.png", "image/png"),
        ] {
            assert_eq!(
                image_mime(Path::new(path)).map(|m| m.to_string()),
                Some(expected.to_string()),
                "{path}"
            );
        }
        assert!(image_mime(Path::new("a.txt")).is_none());
        assert!(image_mime(Path::new("no_extension")).is_none());
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
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
        };
        let kml = fl.as_kml();
        assert_eq!(
            kml,
            "<Placemark><name>test_files/sunrise.jpg</name><Point><coordinates>12.345,45.6789,46.79</coordinates></Point><TimeStamp><when>2025-03-06T05:41:42</when></TimeStamp></Placemark>"
        );
    }

    #[test]
    fn test_kml_timestamp_roundtrip() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: None,
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
        };
        let kml = Kml::from_str(&fl.as_kml()).unwrap();
        let back = FileLocation::from_kml_element(&kml).unwrap();
        assert_eq!(back.timestamp, fl.timestamp);
    }

    #[test]
    fn test_as_geojson() {
        let fl = FileLocation {
            file: "test_files/sunrise.jpg".to_string(),
            latitude: lat(45.6789),
            longitude: lon(12.345),
            altitude: Some(46.79),
            direction: Some(11.0),
            thumbnail: Some("base64".to_string()),
            timestamp: Some("2025:03:06 05:41:42".to_string()),
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
            thumbnail: Some("base64data".to_string()),
            timestamp: Some("2025:03:06 05:41:42".to_string()),
        };
        let v: serde_json::Value = serde_json::from_str(&fl.as_geojson().unwrap()).unwrap();
        let back = FileLocation::from_geojson_feature(&v).unwrap();
        assert_eq!(back.file, fl.file);
        assert_eq!(back.latitude, fl.latitude);
        assert_eq!(back.longitude, fl.longitude);
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
            thumbnail: None,
            timestamp: None,
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
            thumbnail: None,
            timestamp: Some("2025:03:06 05:41:42".to_string()),
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
    fn test_as_kml_escapes_name() {
        let fl = FileLocation {
            file: r#"a & b <tag> "q" 'x'.jpg"#.to_string(),
            latitude: lat(1.0),
            longitude: lon(2.0),
            altitude: None,
            direction: None,
            thumbnail: None,
            timestamp: None,
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
