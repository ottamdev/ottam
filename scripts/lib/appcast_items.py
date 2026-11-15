#!/usr/bin/env python3
"""Print one tab-separated line per appcast item: bundle version, enclosure URL,
enclosure length, and the enclosure's EdDSA signature (empty when missing)."""
import sys
import xml.etree.ElementTree as ET

SPARKLE = '{http://www.andymatuschak.org/xml-namespaces/sparkle}'


def items(path):
    for item in ET.parse(path).getroot().iter('item'):
        enclosure = item.find('enclosure')
        attributes = enclosure.attrib if enclosure is not None else {}
        yield (
            (item.findtext(SPARKLE + 'version') or '').strip(),
            attributes.get('url', ''),
            attributes.get('length', ''),
            attributes.get(SPARKLE + 'edSignature', ''),
        )


if __name__ == '__main__':
    if len(sys.argv) != 2:
        sys.exit('usage: appcast_items.py <appcast.xml>')
    for row in items(sys.argv[1]):
        print('\t'.join(row))
