# Build full runtime image

```bash
DOCKER_BUILDKIT=1 docker build \
-f docker/Dockerfile.ubuntu24-build \
--build-arg AGAVE_REPO=https://github.com/neonlabsorg/agave.git \
--build-arg AGAVE_REF=gasless-parameters \
-t agave-runtime .
```

# Export binaries to host bin/ without keeping an image

```bash
DOCKER_BUILDKIT=1 docker build \
-f docker/Dockerfile.ubuntu24-build \
--target artifacts \
--output type=local,dest=./bin \
.

# Notes
# - The Dockerfile defaults to cloning over HTTPS. If you need SSH (for a private fork),
#   pass `--ssh default --build-arg AGAVE_REPO=git@github.com:<org>/<repo>.git`.
```
