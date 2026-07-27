# Unless explicitly stated otherwise all files in this repository are licensed
# under the Apache 2.0 License. This product includes software developed at
# Datadog (https://www.datadoghq.com/).
#
# Copyright 2024-Present Datadog, Inc.

FROM amazonlinux:2023

# AL2023's default nginx (1.28.2+) lacks a published Datadog module in S3.
# Pin to 1.28.0 via nginx.org's stable repo (the latest version with a module).
# module_hotfixes=true disables DNF module-stream checks that would otherwise
# block installing a RHEL9-targeted package on AL2023.
RUN printf '[nginx-stable]\nname=nginx stable repo\nbaseurl=http://nginx.org/packages/rhel/9/$basearch/\ngpgcheck=1\nenabled=1\ngpgkey=https://nginx.org/keys/nginx_signing.key\nmodule_hotfixes=true\n' \
      > /etc/yum.repos.d/nginx.repo && \
    rpm --import https://nginx.org/keys/nginx_signing.key && \
    dnf -y install nginx-1.28.0 tar
RUN dnf -y swap gnupg2-minimal gnupg2-full
