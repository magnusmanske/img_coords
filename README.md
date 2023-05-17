# ImageCoordinates
This executable can scan a directory tree on your disk, find all images with EXIF data containing coordinates, and collect them into a single file.
Output can be either JSON (default) or KML. JSON also contains the camera direction, if available in EXIF.

# Installation
```
cargo install img_coords
```
or checkout and build the repo manually.

## Uninstall
```
cargo uninstall img_coords
```


# Example
```
img_coords scan --dir SOME_ROOT_DIRECTORY --format kml
```

Run `img_coords` or `img_coords scan` to get help.