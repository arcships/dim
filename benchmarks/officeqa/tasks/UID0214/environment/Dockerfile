# Never built. Every OfficeQA Pro task runs the prebuilt corpus image named by
# `docker_image` in task.toml; this file exists only because harbor requires an
# environment definition. If harbor ever builds it, fail loudly instead of
# running a task with no corpus.
FROM ubuntu:24.04
RUN echo "OfficeQA Pro needs the prebuilt corpus image (force_build must be false)" && exit 1
