#!/usr/bin/env python3
"""Select expensive PR checks from all changed paths, including deleted paths."""
import argparse
import subprocess


def surfaces(paths):
    rust = swift = docs = False
    for path in paths:
        if path.endswith('.md') or path in ('scripts/check-docs.sh', 'scripts/lib/check_docs.py'):
            docs = True
            continue
        shared = path.startswith(('.github/workflows/', '.githooks/', 'scripts/'))
        toolchain = path in ('rust-toolchain.toml', 'rust-toolchain')
        rust |= path.startswith(('backend/', 'app/')) or shared or toolchain
        swift |= path.startswith(('app/', 'backend/')) or shared or toolchain
    return {'rust': rust, 'swift': swift, 'docs': docs}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--base')
    parser.add_argument('--head', default='HEAD')
    args = parser.parse_args()
    if args.base:
        diff = subprocess.check_output([
            'git', 'diff', '--name-only', '--no-renames', '-z',
            args.base + '...' + args.head, '--',
        ])
        checks = surfaces(diff.decode().rstrip('\0').split('\0'))
    else:
        checks = dict.fromkeys(('rust', 'swift', 'docs'), True)
    for name, enabled in checks.items():
        print(f'{name}={str(enabled).lower()}')
