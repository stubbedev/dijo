lib: file:
lib.listToAttrs (
  map (
    line:
    let
      pair = builtins.match "([A-Z0-9_]+)=(.+)" line;
    in
    lib.nameValuePair (builtins.elemAt pair 0) (builtins.elemAt pair 1)
  ) (builtins.filter (line: line != "") (lib.splitString "\n" (builtins.readFile file)))
)
