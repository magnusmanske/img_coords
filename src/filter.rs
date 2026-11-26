use crate::file_location::{Latitude, Longitude};

/// Mean Earth radius in kilometres.
const EARTH_RADIUS_KM: f64 = 6371.0088;

/// Great-circle distance between two points, in kilometres.
pub fn distance_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (phi1, phi2) = (lat1.to_radians(), lat2.to_radians());
    let delta_phi = phi2 - phi1;
    let delta_lambda = (lon2 - lon1).to_radians();
    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_KM * a.sqrt().clamp(-1.0, 1.0).asin()
}

/// A latitude/longitude bounding box, inclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bbox {
    min_lon: f64,
    min_lat: f64,
    max_lon: f64,
    max_lat: f64,
}

impl Bbox {
    /// Parses `MIN_LON,MIN_LAT,MAX_LON,MAX_LAT`.
    pub fn parse(text: &str) -> Option<Self> {
        let values = text
            .split(',')
            .map(|part| part.trim().parse::<f64>().ok())
            .collect::<Option<Vec<_>>>()?;
        let [min_lon, min_lat, max_lon, max_lat] = values.as_slice() else {
            return None;
        };
        if !(-180.0..=180.0).contains(min_lon)
            || !(-180.0..=180.0).contains(max_lon)
            || !(-90.0..=90.0).contains(min_lat)
            || !(-90.0..=90.0).contains(max_lat)
            || min_lon > max_lon
            || min_lat > max_lat
        {
            return None;
        }
        Some(Self {
            min_lon: *min_lon,
            min_lat: *min_lat,
            max_lon: *max_lon,
            max_lat: *max_lat,
        })
    }

    pub fn contains(&self, latitude: Latitude, longitude: Longitude) -> bool {
        (self.min_lat..=self.max_lat).contains(&latitude.get())
            && (self.min_lon..=self.max_lon).contains(&longitude.get())
    }
}

/// A circular area around a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Near {
    latitude: f64,
    longitude: f64,
    radius_km: f64,
}

impl Near {
    /// Parses `LAT,LON,RADIUS_KM`.
    pub fn parse(text: &str) -> Option<Self> {
        let values = text
            .split(',')
            .map(|part| part.trim().parse::<f64>().ok())
            .collect::<Option<Vec<_>>>()?;
        let [latitude, longitude, radius_km] = values.as_slice() else {
            return None;
        };
        if !(-90.0..=90.0).contains(latitude)
            || !(-180.0..=180.0).contains(longitude)
            || *radius_km <= 0.0
            || !radius_km.is_finite()
        {
            return None;
        }
        Some(Self {
            latitude: *latitude,
            longitude: *longitude,
            radius_km: *radius_km,
        })
    }

    pub fn contains(&self, latitude: Latitude, longitude: Longitude) -> bool {
        distance_km(
            self.latitude,
            self.longitude,
            latitude.get(),
            longitude.get(),
        ) <= self.radius_km
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lat(value: f64) -> Latitude {
        Latitude::new(value).unwrap()
    }

    fn lon(value: f64) -> Longitude {
        Longitude::new(value).unwrap()
    }

    #[test]
    fn test_bbox_parse_and_contains() {
        let bbox = Bbox::parse("10,40,20,50").unwrap();
        assert!(bbox.contains(lat(45.0), lon(15.0)));
        assert!(!bbox.contains(lat(55.0), lon(15.0)));
        assert!(!bbox.contains(lat(45.0), lon(25.0)));
        assert!(Bbox::parse("20,40,10,50").is_none());
        assert!(Bbox::parse("1,2,3").is_none());
    }

    #[test]
    fn test_near_distance() {
        let near = Near::parse("0,0,120").unwrap();
        assert!(near.contains(lat(0.0), lon(0.0)));
        // One degree of latitude is roughly 111 km.
        assert!(near.contains(lat(1.0), lon(0.0)));
        assert!(!near.contains(lat(2.0), lon(0.0)));
        assert!(Near::parse("0,0,-1").is_none());
    }
}
