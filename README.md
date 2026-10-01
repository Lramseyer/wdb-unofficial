# Unofficial parser of WDB waveform dump files

## Disclaimer and Acknowledgements

This project is not affilited in any way with AMD/Xilinx. This project and all information in the documentation have been obtained by black-box fuzzing.

This project is a continulation of the work of Filip Filmar, who originally documented the WDB format, and created the wdbcvt tool used to verify this library.

- Paper on the documentation process: (https://www.hdlfactory.com/wdbcvt/wdbcvt-report.pdf)[https://www.hdlfactory.com/wdbcvt/wdbcvt-report.pdf]
- Blog post: (https://www.hdlfactory.com/post/2026/09/04/wdbcvt-reading-vivado-wdb/)[https://www.hdlfactory.com/post/2026/09/04/wdbcvt-reading-vivado-wdb/]
- WDB to FST converter: (https://github.com/filmil/wdbcvt)[https://github.com/filmil/wdbcvt]

## Objectives

AMD/Xilinx has not provided any documentation on the WDB waveform dump format, which requires users to use Vivado to view waveform dumps. Historically, it was not possible to use open source viewers to view WDB files.

## Goals

1. Document the WDB format in a easily readable manner
2. Create a parser in Rust and use wdbcvt to validate my parser
3. Turn the code into a Rust crate that can be used in other projects