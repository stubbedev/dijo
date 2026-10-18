# The container image around a statically linked executable (the flake's static executable
# without Tika, or ci/static-image.sh's with it, through ci/container.sh): the executable as
# /extractous with nothing else, run as an unprivileged user that may write only to /tmp and its
# cache, serving on port 9998. Building it runs the executable once, so an executable for another
# machine or one that needs a shared library fails here.
{ pkgs, executable }:
pkgs.dockerTools.buildLayeredImage {
  name = "extractous";
  tag = "local";
  fakeRootCommands = ''
    cp ${executable} extractous
    mkdir -p tmp var/cache/extractous
    chmod 1777 tmp
    chown 10001:10001 var/cache/extractous
    printf 'probe\n' > "$TMPDIR/probe.txt"
    XDG_CACHE_HOME="$TMPDIR" ./extractous detect "$TMPDIR/probe.txt"
  '';
  config = {
    Entrypoint = [ "/extractous" ];
    Cmd = [ "serve" ];
    User = "10001:10001";
    Env = [
      "XDG_CACHE_HOME=/var/cache"
      "EXTRACTOUS_LISTEN=0.0.0.0:9998"
    ];
    ExposedPorts."9998/tcp" = { };
  };
}
