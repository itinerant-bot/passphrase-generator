# Bundled dictionaries

Each file is an adaptation of an established passphrase list: dice-roll columns
are removed; only ASCII alphabetic entries of at least four characters are kept;
spellings are lowercased, sorted, and deduplicated. Accents and umlauts are not
transliterated. The application also deduplicates across selected languages.

## Sources and attribution

| File | Words | Original source and creator | Dictionary license |
| --- | ---: | --- | --- |
| `english.txt` | 7,690 | [EFF large word list](https://www.eff.org/files/2016/07/18/eff_large_wordlist.txt), Electronic Frontier Foundation, 2016; [Joseph Bonneau's introduction](https://www.eff.org/deeplinks/2016/07/new-wordlists-random-passphrases) | CC BY 4.0, under [EFF's copyright policy](https://www.eff.org/copyright) |
| `french.txt` | 7,765 | [Alternative French Diceware list](https://github.com/ArthurPons/diceware-fr-alt), Arthur Pons, 2018; compiled using the Lexique 3 database | MIT; [original notice](licenses/french-MIT.txt) |
| `german.txt` | 7,776 | [Mnemonische deutsche Wortlisten, de-7776-v1](https://github.com/dys2p/wordlists-de), dys2p | CC0 1.0, chosen from the upstream Unlicense / CC0 / BSD-3 options; [original license file](licenses/german.txt) |
| `spanish.txt` | 8,192 | [Spanish Diceware list](https://github.com/ulif/diceware/blob/89534ea7b2c2d090e76307d55497a27ff90cc3d9/diceware/wordlists/wordlist_es.txt), victordargallo, 2024; distributed by ulif/diceware | CC BY 4.0; [notice and license](licenses/spanish-CC-BY-4.0.txt) |
| `latin.txt` | 7,776 | [Latin Diceware list](https://theworld.com/~reinhold/diceware_latin.txt.asc), Sebastian Możejko | CC-BY, as stated on the [upstream Diceware page](https://theworld.com/~reinhold/diceware.html#languages); upstream does not specify a version; [attribution](licenses/latin-CC-BY.txt) |

English license text: [CC BY 4.0](licenses/english-CC-BY-4.0.txt).
These licenses apply to the dictionary data. The modifications described above
were made for four-words; the original authors do not endorse this application.

The French source contains 7,776 records, of which 11 fail the ASCII alphabetic
filter. The English source contains 7,776 records, of which 86 fail the filter.

Imported on 2026-10-11. French revision:
`ee78354f38565945b328552b4861179c40608aee`. German revision:
`6ef31b9aefb8735a7b066592393d12843ec502cd`.

Spanish and Latin imported on 2026-10-21. Spanish revision:
`89534ea7b2c2d090e76307d55497a27ff90cc3d9`. Latin is an unversioned upstream
file, pinned by the source hash below. Both new sources retain all their words
after filtering. The importer excludes PGP signatures from clear-signed lists.

## Reproduce

The importer is a standalone Rust program using only the standard library.
Download the upstream files to the temporary paths below, then run the importer.
Downloading is only needed to update data; building and running the application
never download dictionaries.

```sh
curl -fsSL https://www.eff.org/files/2016/07/18/eff_large_wordlist.txt -o /tmp/four-words-en-source.txt
curl -fsSL https://raw.githubusercontent.com/ArthurPons/diceware-fr-alt/ee78354f38565945b328552b4861179c40608aee/diceware-fr-alt.txt -o /tmp/four-words-fr-source.txt
curl -fsSL https://raw.githubusercontent.com/dys2p/wordlists-de/6ef31b9aefb8735a7b066592393d12843ec502cd/de-7776-v1.txt -o /tmp/four-words-de-source.txt
curl -fsSL https://raw.githubusercontent.com/ulif/diceware/89534ea7b2c2d090e76307d55497a27ff90cc3d9/diceware/wordlists/wordlist_es.txt -o /tmp/four-words-es-source.txt
curl -fsSL https://theworld.com/~reinhold/diceware_latin.txt.asc -o /tmp/four-words-la-source.txt
rustc --edition 2024 tools/import_dictionary.rs -o /tmp/four-words-import-dictionary
/tmp/four-words-import-dictionary /tmp/four-words-en-source.txt wordlists/english.txt
/tmp/four-words-import-dictionary /tmp/four-words-fr-source.txt wordlists/french.txt
/tmp/four-words-import-dictionary /tmp/four-words-de-source.txt wordlists/german.txt
/tmp/four-words-import-dictionary /tmp/four-words-es-source.txt wordlists/spanish.txt
/tmp/four-words-import-dictionary /tmp/four-words-la-source.txt wordlists/latin.txt
```

SHA-256 of the downloaded source bytes:

```text
addd35536511597a02fa0a9ff1e5284677b8883b83e986e43f15a3db996b903e  English
cdb1a1fb6a24c9c9edf46b442110793d2a00f2f4366685e428e49b9a075bf17e  French
440fa02c65591328d6351435d3824c27b483a049f4eca0b13456d8c5090442e7  German
20e7e54f4d1fa1d7e23aca0d11002ef42b1ac140ff9f3494b8e2f5f0ad686c35  Spanish
f3d7a4f107c84451644e3486dc622fb4f0b807fbda1d645369e07f6af98198e7  Latin
```

SHA-256 of the generated dictionary files:

```text
e8a26925af2eb984ec55eadd45c3033e004672f23ee2c9a0285037fe22b33355  english.txt
a4e0e60a91831280d7a70458dbd4fd9be06e0a8549dd67fc8111cc5741c6b8b6  french.txt
199941eea4269a6fe1aa6de9ae6fc18e9280e6fe2c946e736ab33786feca9417  german.txt
20e7e54f4d1fa1d7e23aca0d11002ef42b1ac140ff9f3494b8e2f5f0ad686c35  spanish.txt
b4565c386cfc8a46d959afcee6570d862716e07c3f174a430954883da4865674  latin.txt
```
