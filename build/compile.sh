#!/bin/bash

set -e

TARGETS=$1

if [[ ! -f "$TARGETS" ]]; then
   echo "usage: ./compile.sh targets.[host].json"
   exit 1
fi

TEMPDIR=${TEMPDIR:="$(dirname $0)/tmp"}
TEMPDIR=`cd $TEMPDIR;pwd`

if [[ -f $HOME/.cargo/env ]]; then
   source $HOME/.cargo/env
fi

echo "Parsing targets..."
SCRIPT_DIR=$(dirname "$0")
SCRIPT_DIR=`cd $SCRIPT_DIR;pwd`
TARGETS=$(cat $TARGETS | jq -r '.[] | [.musl_target, .musl_target_docker, .openssl_target, .libsodium_target, .cc, .ldflags, .cflags, .rust_target] | @tsv')
TARGETS=${TARGETS//$'\t'/,}
TARGETS=${TARGETS//$'\r'/,}

mkdir -p $SCRIPT_DIR/artifacts

echo "$TARGETS" | while IFS=',' read -r MUSL_TARGET MUSL_TARGET_DOCKER OPENSSL_TARGET LIBSODIUM_TARGET CC LDFLAGS CFLAGS RUST_TARGET
do
   echo "Building $RUST_TARGET..."

   export CC
   export LD="$CC"
   export LDFLAGS
   export CFLAGS
   export MUSL_PREFIX=""

   echo "Installing rust target..."
   rustup target add $RUST_TARGET


   if [[ ! -z "$MUSL_TARGET" ]]; then
      cd $TEMPDIR/musl-cross-make
      export MUSL_PREFIX="$TEMPDIR/musl-$MUSL_TARGET"

      if [[ -x $(command -v docker) ]]; then
         echo "Fetching pre-built musl-cross..."
         docker run --rm -v$MUSL_PREFIX:/target \
            $MUSL_TARGET_DOCKER \
            cp -arv /usr/local/musl/. /target/
      else 
         echo "Compiling musl-cross..."
         TARGET="$MUSL_TARGET" OUTPUT="$MUSL_PREFIX" make clean install
      fi

      export CC="$MUSL_PREFIX/bin/$MUSL_TARGET-gcc"
      export LD="$MUSL_PREFIX/bin/$MUSL_TARGET-gcc"
      export LDFLAGS="-L$MUSL_PREFIX/lib $LDFLAGS"
      export CFLAGS="-I$MUSL_PREFIX/include $CFLAGS"
   fi

   cat > $SCRIPT_DIR/../dmp-client/.cargo/config << EOF
[target.$RUST_TARGET]
linker = "$CC"
EOF

   OPENSSL_BUILD_DIR=$TEMPDIR/build/openssl-$RUST_TARGET
   if [[ ! -d "$OPENSSL_BUILD_DIR/lib" ]]; then
      echo "Compiling OpenSSL..."
      mkdir -p $OPENSSL_BUILD_DIR
      cd $TEMPDIR/openssl/
     
      if [[ "$OSTYPE"  == "msys" ]]; then
         C:\\tools\\msys64\\bin\\bash.exe -c "perl ./Configure shared no-async $OPENSSL_TARGET --openssldir=$OPENSSL_BUILD_DIR --prefix=$OPENSSL_BUILD_DIR"
         nmake clean install_sw 
      else
         ./Configure shared no-async $OPENSSL_TARGET --openssldir=$OPENSSL_BUILD_DIR --prefix=$OPENSSL_BUILD_DIR
         make clean install_sw
      fi
   fi

   LIBSODIUM_BUILD_DIR=$TEMPDIR/build/libsodium-$RUST_TARGET
   if [[ ! -d "$LIBSODIUM_BUILD_DIR/lib" ]]; then
      echo "Compiling Libsodium..."
      mkdir -p $LIBSODIUM_BUILD_DIR
      cd $TEMPDIR/libsodium/
      unset LD
      ./configure --host=$LIBSODIUM_TARGET --prefix=$LIBSODIUM_BUILD_DIR
      make clean install
      export LD="$CC"
   fi

   export OPENSSL_LIB_DIR="$OPENSSL_BUILD_DIR/lib"
   export OPENSSL_INCLUDE_DIR="$OPENSSL_BUILD_DIR/include"
   export SODIUM_LIB_DIR="$LIBSODIUM_BUILD_DIR/lib"
   export OPENSSL_STATIC=1
   export SODIUM_STATIC=1
   export PKG_CONFIG_ALL_STATIC=1

   echo "Compiling dmp-client for $RUST_TARGET..."
   cd $SCRIPT_DIR/../dmp-client
   cargo build --release --target $RUST_TARGET
   cp $SCRIPT_DIR/../target/$RUST_TARGET/release/client $SCRIPT_DIR/artifacts/client-$RUST_TARGET
done