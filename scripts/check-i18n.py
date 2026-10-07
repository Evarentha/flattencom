#!/usr/bin/env python3
# flattencom - Translation Coverage Checker
#
# Checks catalog duplicates, source message coverage and formatting placeholders.
#
# Authors:
# worryzu <worryzu@gmail.com> @LinearTeam
#
# Copyright (C) 2026 Evarentha
# SPDX-License-Identifier: GPL-3.0-or-later

"""Check English GUI source keys, catalog coverage and translation placeholders."""
import json,re,sys
from pathlib import Path
root=Path(__file__).resolve().parents[1]
errors=[]
def unique_entries(pairs):
    result={}
    for key,value in pairs:
        if key in result:errors.append(f'Duplicate translation key: {key}')
        result[key]=value
    return result
catalog=json.loads((root/'gui/resources/zh_CN.json').read_text(encoding='utf-8'),object_pairs_hook=unique_entries)
used=set()
for file in (root/'gui/src').glob('*.cpp'):
    source=file.read_text(encoding='utf-8')
    pattern=r'fc::text\(\s*((?:"(?:[^"\\]|\\.)*"\s*)+)\)'
    if file.name=='Style.cpp':pattern=r'(?<![\w:])(?:fc::)?text\(\s*((?:"(?:[^"\\]|\\.)*"\s*)+)\)'
    for match in re.finditer(pattern,source):
        key=''.join(json.loads(part) for part in re.findall(r'"(?:[^"\\]|\\.)*"',match[1]));used.add(key)
        if re.search(r'[\u4e00-\u9fff]',key):errors.append(f'{file.name}: non-English source key {key}')
        if key not in catalog:errors.append(f'{file.name}: missing Chinese translation: {key}')
for en,zh in catalog.items():
    if sorted(re.findall(r'%\d+',en))!=sorted(re.findall(r'%\d+',zh)):errors.append(f'Qt placeholder mismatch: {en}')
rust=json.loads((root/'crates/flattencom-core/locales/zh-CN.json').read_text(encoding='utf-8'),object_pairs_hook=unique_entries)
def placeholders(text):return sorted(re.findall(r'(?<!\{)\{([^{}]*)\}(?!\})',text))
for en,zh in rust.items():
    if placeholders(en)!=placeholders(zh):errors.append(f'Rust placeholder mismatch: {en}')
# Check call sites as well as catalog entries: otherwise a missing key silently
# falls back to English. Generated format branches are checked at their callers.
for file in (root/'crates').glob('*/src/**/*.rs'):
    if file.name=='messages.rs':continue
    source=file.read_text(encoding='utf-8').split('#[cfg(test)]')[0]
    for match in re.finditer(r'(?:i18n::text\(|\btr!\()\s*("(?:[^"\\]|\\.)*")',source):
        key=json.loads(match[1])
        if key not in rust:errors.append(f'{file}: missing Chinese translation: {key}')
    if file.name=='tools.rs' and file.parent.parent.name=='flattencom-mcp':
        for match in re.finditer(r'\bdescription\s*=\s*("(?:[^"\\]|\\.)*")',source):
            key=json.loads(match[1])
            if key not in rust:errors.append(f'{file}: missing Chinese tool description: {key}')
for file in [root/'README.md',*(root/'docs').glob('*.md')]:
    content=file.read_text(encoding='utf-8')
    # The language switcher is navigation, in Markdown or centered HTML form.
    if file==root/'README.md':content=re.sub(r'\[简体中文\]\([^)]*\)|<a\s+href="[^"]*"[^>]*>简体中文</a>','',content)
    if re.search(r'[\u4e00-\u9fff]',content):errors.append(f'Non-English default documentation: {file.name}')
if errors:print('\n'.join(errors));sys.exit(1)
print(f'Localization checks passed: {len(used)} GUI keys, {len(rust)} Rust messages.')
