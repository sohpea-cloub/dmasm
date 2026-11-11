# Source me, from bash, with MSVC_SDK set to the directory fetch-sdk.sh filled:
#
#     export MSVC_SDK=DIRECTORY
#     . ci/msvc/env.sh
#
# Has cargo build for i686-pc-windows-msvc on Linux: LLVM's lld-link (the lld
# package) links, with the libraries in MSVC_SDK. Then:
#
#     cargo build --target i686-pc-windows-msvc

: "${MSVC_SDK:?set MSVC_SDK to the directory ci/msvc/fetch-sdk.sh filled}"
MSVC_SDK=$(cd "$MSVC_SDK" && pwd)
export MSVC_SDK

# lld-link by that name, or the newest of those installed with a version
CARGO_TARGET_I686_PC_WINDOWS_MSVC_LINKER=$(command -v lld-link || ls /usr/lib/llvm-*/bin/lld-link 2>/dev/null | sort -V | tail -n 1)
export CARGO_TARGET_I686_PC_WINDOWS_MSVC_LINKER
# The C runtime is linked in, so the programs need no Visual C++ redistributable
export CARGO_TARGET_I686_PC_WINDOWS_MSVC_RUSTFLAGS="-C target-feature=+crt-static -Lnative=$MSVC_SDK/crt/lib/x86 -Lnative=$MSVC_SDK/sdk/lib/um/x86 -Lnative=$MSVC_SDK/sdk/lib/ucrt/x86"
