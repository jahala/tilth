FROM gcr.io/oss-fuzz-base/base-builder-rust

RUN git clone --depth 1 https://github.com/jahala/tilth $SRC/tilth
WORKDIR $SRC/tilth
COPY build.sh $SRC/
