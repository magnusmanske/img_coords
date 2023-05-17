use exif::{Exif, Tag, In, Value};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileLocation {
    pub file: String,
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>, // ATTENTION sea level implied, not checked
    pub direction: Option<f64>, // ATTENTION magnetic direction implied, not checked
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
        format!("<Placemark><name>{}</name><Point><coordinates>{},{},{}</coordinates></Point></Placemark>",
            self.name_xml_escaped(),
            self.longitude,
            self.latitude,
            self.altitude.unwrap_or(0.0),
        )
    }

    pub fn as_geojson(&self) -> String {
        json!({
            "type": "Feature",
           "geometry": {
               "type": "Point",
               "coordinates": [self.longitude, self.latitude]
           },
           "properties": {
               "name": self.file.to_owned()
           }
        }).to_string()
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
        Some(Self {
            file: file.to_string(),
            latitude: Self::lat_from_value(&exif.get_field(Tag::GPSLatitude, In::PRIMARY)?.value,lat_ref)?,
            longitude: Self::lon_from_value(&exif.get_field(Tag::GPSLongitude, In::PRIMARY)?.value,lon_ref)?,
            altitude: Self::f64_from_value(&exif.get_field(Tag::GPSAltitude, In::PRIMARY)?.value),
            direction: Self::f64_from_value(&exif.get_field(Tag::GPSImgDirection, In::PRIMARY)?.value),
        })
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