#!/bin/bash
# No macOS extended attributes in the tarball, or tar on the instance warns about unknown header keywords
COPYFILE_DISABLE=1 tar --no-xattrs -czvf sources/source.tar.gz --exclude='sources/cex_ws/target/' sources/cex_ws/
