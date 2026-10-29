# The OfficeQA Pro task environment: the parsed Treasury Bulletin corpus plus the
# python3 the verifier needs. Built once by build.sh and referenced by digest from
# every task.toml, so the 460 MB corpus is pulled once rather than per task.
FROM ubuntu:24.04
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY corpus/ /app/corpus/
RUN cd /app/corpus && find . -name '*.txt' -type f | sed 's|^\./||' | sort > index.txt
CMD ["/bin/bash"]
