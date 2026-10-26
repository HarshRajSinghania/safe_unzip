#!/bin/sh
set -eu

tag=${1:-${GITHUB_REF_NAME:-}}
version=${tag#v}

if [ -z "${tag}" ] || [ "${tag}" = "${version}" ]; then
    echo "usage: $0 v<version>" >&2
    exit 1
fi

package_version() {
    awk '
        $0 == "[package]" { in_package = 1; next }
        in_package && /^version = / {
            gsub(/^version = "|"$/, "")
            print
            exit
        }
    ' "$1"
}

project_version() {
    awk '
        $0 == "[project]" { in_project = 1; next }
        in_project && /^version = / {
            gsub(/^version = "|"$/, "")
            print
            exit
        }
    ' "$1"
}

rust_version=$(package_version Cargo.toml)
python_rust_version=$(package_version python/Cargo.toml)
python_version=$(project_version python/pyproject.toml)
module_version=$(sed -n 's/^__version__ = "\([^"]*\)"/\1/p' python/python/safe_unzip/__init__.py)

for value in "${rust_version}" "${python_rust_version}" "${python_version}" "${module_version}"; do
    if [ "${value}" != "${version}" ]; then
        echo "release version mismatch: tag=${version}, package=${value}" >&2
        exit 1
    fi
done

if ! grep -Eq "^## \[${version}\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" CHANGELOG.md; then
    echo "CHANGELOG.md has no dated ${version} release heading" >&2
    exit 1
fi

echo "release metadata is consistent for ${version}"
