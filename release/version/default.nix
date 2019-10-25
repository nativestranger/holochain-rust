{ pkgs, config }:
let
 name = "hc-release-hook-version";

 script = pkgs.writeShellScriptBin name ''
for dep in \
 holochain_core_types \
 holochain_core \
 holochain_wasm_utils \
 holochain_common \
 holochain_conductor_lib \
 holochain_dpki \
 holochain_net \
 holochain_conductor_lib_api \
 hdk \
 sim2h
do
 echo "bumping $dep dependency versions to ${config.release.version.current} in all Cargo.toml"
 find . \
  -name "Cargo.toml" \
  -not -path "**/target/**" \
  -not -path "**/.git/**" \
  -not -path "**/.cargo/**" | xargs -I {} \
  sed -i 's/^'"''${dep}"' = { version = "=[0-9]\+.[0-9]\+.[0-9]\+\(-alpha[0-9]\+\)\?"/'"''${dep}"' = { version = "=${config.release.version.current}"/g' {}
done
'';
in
{
 buildInputs = [ script ];
}