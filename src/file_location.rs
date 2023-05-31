use std::{fs::File, io::{BufReader, Cursor}};
use base64::{engine::general_purpose, Engine};
use thumbnailer::{create_thumbnails, ThumbnailSize};
use exif::{Exif, Tag, In, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;
use geojson::GeoJson;
use kml::Kml;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileLocation {
    pub file: String,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>, // ATTENTION sea level implied, not checked
    pub direction: Option<f64>, // ATTENTION magnetic direction implied, not checked
    pub thumbnail: Option<String>,
    pub timestamp: Option<String>,
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
        if let Some(timestamp)=&self.timestamp {
            other += &format!("<TimeStamp><when>{}</when></TimeStamp>",timestamp.replace(' ',"T"));
        }
        format!("<Placemark><name>{}</name><Point><coordinates>{},{},{}</coordinates></Point>{other}</Placemark>",
            self.name_xml_escaped(),
            self.longitude,
            self.latitude,
            self.altitude.unwrap_or(0.0),
        )
    }

    pub fn as_geojson(&mut self) -> String {
        let mut j = json!({
            "type": "Feature",
           "geometry": {
               "type": "Point",
               "coordinates": [self.longitude, self.latitude]
           },
           "properties": {
               "name": self.file.to_owned(),
           }
        });
        if let Some(altitude)=self.altitude {
            j["properties"]["altitude"] = json!(altitude);
        }
        if let Some(direction)=self.direction {
            j["properties"]["direction"] = json!(direction);
        }
        if let Some(timestamp)=&self.timestamp {
            j["properties"]["timestamp"] = json!(timestamp);
        }
        if let Some(base64)=&self.thumbnail {
            j["properties"]["thumbnail"] = json!(base64)
        }
        j.to_string()
    }

    pub fn generate_missing_thumbnail(&mut self) {
        if self.thumbnail.is_none() {
            self.thumbnail = self.get_thumbnail_base64();
        }
    }

    fn get_thumbnail_base64(&self) -> Option<String> {
        let file = File::open(&self.file).ok()?;
        let reader = BufReader::new(file);
        let thumbnail = create_thumbnails(reader, mime::IMAGE_JPEG, [ThumbnailSize::Medium]).ok()?;
        let thumbnail = thumbnail.get(0)?.to_owned();
        let mut buf = Cursor::new(Vec::new());
        thumbnail.write_jpeg(&mut buf,8).ok()?;
        let vec = buf.into_inner();
        let encoded: String = general_purpose::STANDARD_NO_PAD.encode(vec);
        Some(encoded)
    }

    pub fn from_kml_element(element: &Kml) -> Option<Self> {
        if let Kml::Placemark(pm) = element {
            if let (Some(name),Some(geometry)) = (&pm.name,&pm.geometry) {
                if let kml::types::Geometry::Point(point) = geometry {
                    let ret = Self {
                        file: name.to_owned(),
                        latitude: point.coord.y,
                        longitude: point.coord.x,
                        altitude: point.coord.z,
                        direction: None, // Not encoded in KML
                        thumbnail: None,
                        timestamp: None,
                    };
                    return Some(ret);
                }
            }
        }
        None
    }

    pub fn from_geojson_feature(v: &serde_json::Value) -> Option<Self> {
        let geojson_str = v.to_string();
        let geojson = geojson_str.parse::<GeoJson>().ok()?;
        let feature = match geojson {
            GeoJson::Feature(feature) => feature,
            _ => return None,
        };
        let point = match feature.geometry?.value {
            geojson::Value::Point(point) => point,
            _ => return None,
        };
        let properties = feature.properties?;
        let thumbnail = match properties.get("thumbnail") {
            Some(s) => match s.as_str() {
                Some(s) => Some(s.to_string()),
                None => None,
            },
            None => None,
        };
        let timestamp = match properties.get("timestamp") {
            Some(s) => match s.as_str() {
                Some(s) => Some(s.to_string()),
                None => None,
            },
            None => None,
        };
        Some(Self{
            file: properties.get("name")?.as_str()?.to_string(),
            latitude: *point.get(1)?,
            longitude: *point.get(0)?,
            altitude: properties.get("altitude")?.as_f64(),
            direction: properties.get("direction")?.as_f64(),
            thumbnail,
            timestamp,
        })
    }

    fn name_xml_escaped(&self) -> String {
        self.file
            .replace('&',"&amp;")
            .replace('<',"&lt;")
            .replace('>',"&gt;")
            .replace('"',"&quot;")
            .replace('\'',"&apos;")
    }

    fn from_exif(file: &str, exif: &Exif) -> Option<Self> {
        let lat_ref = Self::letter_from_value(&exif.get_field(Tag::GPSLatitudeRef, In::PRIMARY)?.value)?;
        let lon_ref = Self::letter_from_value(&exif.get_field(Tag::GPSLongitudeRef, In::PRIMARY)?.value)?;
        let timestamp = Self::string_from_value(exif.get_field(Tag::DateTimeOriginal, In::PRIMARY));
        Some(Self {
            file: file.to_string(),
            latitude: Self::lat_from_value(&exif.get_field(Tag::GPSLatitude, In::PRIMARY)?.value,lat_ref)?,
            longitude: Self::lon_from_value(&exif.get_field(Tag::GPSLongitude, In::PRIMARY)?.value,lon_ref)?,
            altitude: Self::f64_from_value(&exif.get_field(Tag::GPSAltitude, In::PRIMARY)?.value),
            direction: Self::f64_from_value(&exif.get_field(Tag::GPSImgDirection, In::PRIMARY)?.value),
            thumbnail: None,
            timestamp: timestamp,
        })
    }

    fn string_from_value(f: Option<&exif::Field>) -> Option<String> {
        if let Some(f)=f {
            if let Value::Ascii(vs) = &f.value {
                if let Some(s)=vs.get(0) {
                    if let Ok(ts)=std::str::from_utf8(s) {
                        return Some(ts.to_string());
                    }
                }
            }
        }
        None
    }

    fn letter_from_value(v: &Value) -> Option<char> {
        match v {
            Value::Ascii(chars) => Some(*(chars.get(0)?.get(0)?) as char),
            _ => None
        }
    }

    fn f64_from_value(v: &Value) -> Option<f64> {
        match v {
            Value::Rational(r) => Some(r.get(0)?.to_f64()),
            _ => None
        }
    }

    fn lat_from_value(v: &Value, r: char) -> Option<f64> {
        latlon::parse_lat(Self::coord_string_from_value(v, r)?).ok()
    }

    fn lon_from_value(v: &Value, r: char) -> Option<f64> {
        latlon::parse_lng(Self::coord_string_from_value(v, r)?).ok()
    }

    fn coord_string_from_value(v: &Value, r: char) -> Option<String> {
        let v = match v {
            Value::Rational(r) => r,
            _ => return None
        };
        let d = v.get(0)?.to_f64();
        let m = v.get(1)?.to_f64();
        let s = v.get(2)?.to_f64();
        let s = format!("{d}° {m}′ {s}″ {r}");
        Some(s)
    }
}