# A build script fragment that copies the license files at the top of a package's source into
# <dir>/<package name>, failing when the source has none. macOS metadata files (._*) beside a
# tarball's top directory are not part of the source.
dir: p: ''
  target=${dir}/${p.pname or p.name}
  mkdir -p "$target"
  unpacked=$(mktemp -d)
  if [ -d ${p.src} ]; then
    cp -r --no-preserve=mode ${p.src}/. "$unpacked"
  else
    tar -xf ${p.src} -C "$unpacked"
  fi
  root=$unpacked
  if [ "$(find "$unpacked" -mindepth 1 -maxdepth 1 ! -name '._*' | wc -l)" = 1 ]; then
    root=$(find "$unpacked" -mindepth 1 -maxdepth 1 ! -name '._*')
  fi
  find "$root" -maxdepth 1 -type f \( -iname '*licen[cs]e*' -o -iname 'COPYING*' \
    -o -iname 'COPYRIGHT*' -o -iname 'NOTICE*' -o -iname 'README.ijg' \) -exec cp {} "$target"/ \;
  if [ -z "$(ls -A "$target")" ]; then
    echo "no license file found in the source of ${p.name}" >&2
    exit 1
  fi
  rm -rf "$unpacked"
''
