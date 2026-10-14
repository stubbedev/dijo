package ai.yobix;

/**
 * The script a page of a document, read side by side with its other pages, is first read in with
 * the automatic language, set in the page's ParseContext. Such a page neither reads nor teaches
 * the extraction's memory of scripts ({@link OcrConfig#assumedScript()}), so that every page
 * reads the same whichever finishes first; the script it learned, if any, is left in its
 * metadata under {@link #LEARNED} for whoever reads the pages to pass on.
 *
 * @param script the script, or null for the one assumed before any detection
 */
record AssumedScript(String script) {

    static final String LEARNED = "extractous:learned-script";
}
