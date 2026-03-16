#!/usr/bin/env bash

purge_list=()

install_packages() {
    apt-get update
    apt-get install --assume-yes --no-install-recommends "${@}"
    purge_list+=("${@}")
}

purge_packages() {
    if (( ${#purge_list[@]} )); then
        apt-get purge --assume-yes --auto-remove "${purge_list[@]}"
    fi
}
