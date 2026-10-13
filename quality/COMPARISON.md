# Extractous and Apache Tika

The quality corpus extracted by extractous and by tika-app of the same Tika version on the JVM, both with their default options: OCR where there is no text, without a language hint. Each cell is extractous / Tika.

| Document | Category | CER | Token error rate | Recall | Spurious |
|---|---|---|---|---|---|
| prose.odt | born-digital | 0.000 / 0.206 | 0.000 / 0.466 | 1.000 / 0.997 | 0.000 / 0.315 |
| prose.docx | born-digital | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose.doc | born-digital | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose.rtf | born-digital | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose.pdf | born-digital | 0.018 / 0.018 | 0.016 / 0.016 | 1.000 / 1.000 | 0.016 / 0.016 |
| prose.html | born-digital | 0.005 / 0.009 | 0.003 / 0.007 | 1.000 / 1.000 | 0.003 / 0.007 |
| prose.epub | born-digital | 0.031 / 0.035 | 0.030 / 0.033 | 1.000 / 1.000 | 0.029 / 0.032 |
| prose-pandoc.docx | born-digital | 0.001 / 0.001 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-columns.pdf | multi-column | 0.018 / 0.018 | 0.023 / 0.016 | 0.993 / 1.000 | 0.019 / 0.016 |
| prose-columns-reversed.pdf | multi-column | 0.000 / 0.745 | 0.000 / 0.885 | 1.000 / 1.000 | 0.000 / 0.000 |
| report.pdf | headers-footers | 0.006 / 0.017 | 0.007 / 0.018 | 1.000 / 1.000 | 0.007 / 0.017 |
| report.docx | headers-footers | 0.006 / 0.006 | 0.006 / 0.006 | 1.000 / 1.000 | 0.005 / 0.005 |
| german.pdf | script | 0.065 / 0.065 | 0.052 / 0.052 | 1.000 / 1.000 | 0.049 / 0.049 |
| german.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| arabic.pdf | script | 0.150 / 0.838 | 0.161 / 1.000 | 0.893 / 0.054 | 0.000 / 0.946 |
| arabic.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| hebrew.pdf | script | 0.100 / 0.100 | 0.061 / 0.061 | 1.000 / 1.000 | 0.058 / 0.058 |
| hebrew.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| chinese.pdf | script | 0.079 / 0.079 | 0.063 / 0.063 | 1.000 / 1.000 | 0.059 / 0.059 |
| chinese.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| japanese.pdf | script | 0.068 / 0.068 | 0.049 / 0.049 | 1.000 / 1.000 | 0.047 / 0.047 |
| japanese.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| thai.pdf | script | 0.094 / 0.958 | 0.085 / 0.936 | 1.000 / 0.085 | 0.079 / 0.351 |
| thai.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| hindi.pdf | script | 0.074 / 0.832 | 0.060 / 1.000 | 1.000 / 0.060 | 0.056 / 0.938 |
| hindi.docx | script | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| revisions.odt | revisions | 0.000 / 0.305 | 0.000 / 0.367 | 1.000 / 1.000 | 0.000 / 0.167 |
| revisions.docx | revisions | 0.000 / 0.027 | 0.000 / 0.033 | 1.000 / 1.000 | 0.000 / 0.032 |
| revisions.doc | revisions | 0.000 / 0.005 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| table.xlsx | tables | 0.168 / 0.168 | 0.156 / 0.156 | 1.000 / 1.000 | 0.135 / 0.135 |
| table.docx | tables | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| table.html | tables | 0.044 / 0.088 | 0.031 / 0.062 | 1.000 / 1.000 | 0.030 / 0.059 |
| table.pdf | tables | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-scan-300dpi.png | ocr | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-scan-150dpi.png | ocr | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-scan-skewed.png | ocr | 0.000 / 0.950 | 0.000 / 0.951 | 1.000 / 0.049 | 0.000 / 0.000 |
| prose-scan-rotated.png | ocr | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-scan.pdf | ocr | 0.000 / 1.007 | 0.000 / 1.007 | 1.000 / 1.000 | 0.000 / 0.502 |
| prose-photo-upside-down.jpg | ocr | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| arabic-scan-300dpi.png | ocr | 0.125 / 0.866 | 0.107 / 1.000 | 0.893 / 0.000 | 0.000 / 1.000 |
| prose-broken-private-use.pdf | broken-text-layer | 0.000 / 0.987 | 0.000 / 1.000 | 1.000 / 0.000 | 0.000 / 0.000 |
| prose-broken-offset.pdf | broken-text-layer | 0.000 / 0.829 | 0.000 / 1.020 | 1.000 / 0.000 | 0.000 / 1.000 |
| hyphenated.pdf | hyphenation | 0.001 / 0.029 | 0.006 / 0.290 | 1.000 / 0.858 | 0.006 / 0.253 |
| prose-scan-inverted.png | ocr | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| prose-scan-72dpi.png | ocr | 0.000 / 0.012 | 0.000 / 0.046 | 1.000 / 0.957 | 0.000 / 0.039 |
| prose-scan-shadow.png | ocr | 0.000 / 0.312 | 0.000 / 0.331 | 1.000 / 0.669 | 0.000 / 0.038 |
| report-scan.tif | ocr | 0.000 / 0.012 | 0.000 / 0.011 | 1.000 / 1.000 | 0.000 / 0.011 |
| report-scan-skewed.tif | ocr | 0.000 / 0.331 | 0.000 / 0.337 | 1.000 / 0.674 | 0.000 / 0.022 |
| arabic-cp1256.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| hebrew-cp1255.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| thai-cp874.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| japanese-shift_jis.txt | legacy-encoding | 0.000 / 1.008 | 0.000 / 1.082 | 1.000 / 0.000 | 0.000 / 1.000 |
| japanese-euc-jp.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| chinese-gb18030.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| chinese-traditional-big5.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| russian-koi8-r.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| russian-cp1251.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| greek-cp1253.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| polish-cp1250.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| polish-iso-8859-2.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| czech-cp1250.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| turkish-cp1254.txt | legacy-encoding | 0.000 / 0.000 | 0.000 / 0.000 | 1.000 / 1.000 | 0.000 / 0.000 |
| **Mean** | | 0.017 / 0.176 | 0.015 / 0.198 | 0.996 / 0.861 | 0.010 / 0.115 |
