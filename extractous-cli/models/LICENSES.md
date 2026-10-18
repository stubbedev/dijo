## Models embedded in the extractous binary

`langdetect-20260320.bin` is the CharSoup language detection model of Apache Tika 4.1.0
(`org.apache.tika:tika-langdetect-charsoup-core`), licensed under the Apache License 2.0, whose text is extractous's own
`LICENSE`. It is embedded unchanged. Its notice:

````text
Apache Tika langdetect (charsoup core — no Tika dependencies)
Copyright 2007-2026 The Apache Software Foundation

This product includes software developed at
The Apache Software Foundation (http://www.apache.org/).
````


`junkdetect.bin` is the text quality model of Apache Tika 4.1.0's JunkDetector (`org.apache.tika:tika-ml-junkdetect`),
licensed under the Apache License 2.0. It is embedded unchanged. Its notice:

````text
Apache Tika ML junk detector — runtime and training tools
Copyright 2007-2026 The Apache Software Foundation

This product includes software developed at
The Apache Software Foundation (http://www.apache.org/).
````


`nb-bigram.bin` and `utf16-specialist.bin` are the byte bigram charset classifier and the UTF-16 byte order classifier
of Apache Tika 4.1.0's MojibusterEncodingDetector (`org.apache.tika:tika-encoding-detector-mojibuster`), licensed
under the Apache License 2.0. They are embedded unchanged. Their notice:

````text
Apache Tika ML charset encoding detector
Copyright 2007-2026 The Apache Software Foundation

This product includes software developed at
The Apache Software Foundation (http://www.apache.org/).
````


`java-charsets.bin` holds the names, aliases and decoding tables of the charsets of OpenJDK 25, as its decoders
decode them, licensed under the GNU General Public License version 2 with the Classpath Exception.
