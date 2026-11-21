//! `TikaDatesTest`'s cases.

use super::*;

/// `testNormalize`.
const NORMALIZED: &[(&str, &str)] = &[
    ("2006-07-15T00:01:46+01:00", "2006-07-14T23:01:46Z"),
    ("2004-08-09T14:03:21-05:00", "2004-08-09T19:03:21Z"),
    ("2011-01-01T12:41:36Z", "2011-01-01T12:41:36Z"),
    ("2007-01-01T00:07+01:00", "2006-12-31T23:07:00Z"),
    ("2010-03-08T21:37-05:00", "2010-03-09T02:37:00Z"),
    ("2016-08-26T07:06Z", "2016-08-26T07:06:00Z"),
    ("2010-01-10T13:00:19", "2010-01-10T13:00:19Z"),
    ("2013-09-08T04:14:06.006", "2013-09-08T04:14:06Z"),
    ("2018-08-06T20:53:57.9843081+01:00", "2018-08-06T19:53:57Z"),
    ("2011-12-17T21:47:07.88+09:00", "2011-12-17T12:47:07Z"),
    ("2007-02-21T09:10:07.0+2:00", "2007-02-21T07:10:07Z"),
    ("2018-03-15T10:18:57+1:00", "2018-03-15T09:18:57Z"),
    ("2011-12-31T15:37:24.00986+03:30", "2011-12-31T12:07:24Z"),
    ("2010-05-09T21:34:38+0200", "2010-05-09T19:34:38Z"),
    ("2003-01-01 12:00:00 +0100", "2003-01-01T11:00:00Z"),
    ("2010-04-22 15:20 -0400", "2010-04-22T19:20:00Z"),
    ("2015-08-26T09:39", "2015-08-26T09:39:00Z"),
    ("2005-11-03", "2005-11-03T12:00:00Z"),
    ("2017-06-1", "2017-06-01T12:00:00Z"),
    ("1969-7-20", "1969-07-20T12:00:00Z"),
    ("D:20041028184621", "2004-10-28T18:46:21Z"),
    ("D:20080528153425+02'00'", "2008-05-28T13:34:25Z"),
    ("D:20030101120000+05'30'", "2003-01-01T06:30:00Z"),
    ("D:20030101120000+05'30", "2003-01-01T06:30:00Z"),
    ("D:20030101120000-08'00'", "2003-01-01T20:00:00Z"),
    ("D:20030101120000Z", "2003-01-01T12:00:00Z"),
    ("D:20030101120000Z00'00'", "2003-01-01T12:00:00Z"),
    ("D:200301011200", "2003-01-01T12:00:00Z"),
    ("D:20041109", "2004-11-09T12:00:00Z"),
    ("  D:20030101  ", "2003-01-01T12:00:00Z"),
    ("20030101120000", "2003-01-01T12:00:00Z"),
    ("2014:08:21 13:33:53", "2014-08-21T13:33:53Z"),
    ("2015:06:12", "2015-06-12T12:00:00Z"),
    ("2017:12:23 12:51:20Z", "2017-12-23T12:51:20Z"),
    ("Fri, 01 Dec 2006 00:06:00 GMT", "2006-12-01T00:06:00Z"),
    (
        "Fri, 03 Dec 1999 15:17:26 -0500 (EST)",
        "1999-12-03T20:17:26Z",
    ),
    ("01 Aug 2001 11:14:22 -0400", "2001-08-01T15:14:22Z"),
    ("1 Apr 1999 11:40:10 -0500", "1999-04-01T16:40:10Z"),
    ("Fri, 16 Apr 93 08:25:04 -0400", "1993-04-16T12:25:04Z"),
    ("Fri, 4 Sep 98 00:24:11 -0600", "1998-09-04T06:24:11Z"),
    ("Fri, 01 Apr 1994 16:57 -0800 (PST)", "1994-04-02T00:57:00Z"),
    ("Fri, 01 Dec 2000 08:39:07", "2000-12-01T08:39:07Z"),
    (
        "Fri, 18 Jun 1993 9:42:24 -0400 (EDT)",
        "1993-06-18T13:42:24Z",
    ),
    ("10 Jul 1996 13:23 EDT", "1996-07-10T17:23:00Z"),
    ("Fri, 1 Feb 91 18:33:59 PST", "1991-02-02T02:33:59Z"),
    (
        "Thu,  6 May 2004 10:12:09 +0200 (CEST)",
        "2004-05-06T08:12:09Z",
    ),
    (
        "Fri, 12 Apr 2002 18:56:49 -0400 (Eastern Daylight Time)",
        "2002-04-12T22:56:49Z",
    ),
    (
        "Fri, 19 Oct 2007 10:23:52 -0600 (GMT-06:00)",
        "2007-10-19T16:23:52Z",
    ),
    ("Fri, 16 Jan 2004 10:45:33 0000", "2004-01-16T10:45:33Z"),
    ("15 Nov 94 09:44:30 -5", "1994-11-15T14:44:30Z"),
    ("Thursday, 18 December 1997 11:31pm", "1997-12-18T23:31:00Z"),
    ("29 Jan 2014", "2014-01-29T12:00:00Z"),
    ("9 November 2001", "2001-11-09T12:00:00Z"),
    ("3 March, 2017", "2017-03-03T12:00:00Z"),
    ("Friday, April 02, 1999 1:20:07 PM", "1999-04-02T13:20:07Z"),
    ("Fri, Nov 15, 1996 6:58 AM", "1996-11-15T06:58:00Z"),
    (
        "Friday, December 12, 2008, 11:28 AM",
        "2008-12-12T11:28:00Z",
    ),
    ("Fri, May 11, 2012 at 6:06 PM", "2012-05-11T18:06:00Z"),
    ("Monday, August 12, 1996 5:13PM", "1996-08-12T17:13:00Z"),
    ("Fri, Mar 9, 2012 20:23", "2012-03-09T20:23:00Z"),
    ("Monday, October 29, 2001 10:13", "2001-10-29T10:13:00Z"),
    ("Friday, June 2, 2000   3:45 PM PDT", "2000-06-02T22:45:00Z"),
    (
        "Friday, February 19 2010 13:31:33 CET",
        "2010-02-19T12:31:33Z",
    ),
    ("August 12, 1999", "1999-08-12T12:00:00Z"),
    ("April 5, 1989", "1989-04-05T12:00:00Z"),
    ("Aug 13 2008", "2008-08-13T12:00:00Z"),
    ("Fri Jan 18  2008", "2008-01-18T12:00:00Z"),
    ("Wed May 23 2018 17:52:58 GMT+0200", "2018-05-23T15:52:58Z"),
    ("Fri Apr 10 04:00:33 PDT 1992", "1992-04-10T11:00:33Z"),
    ("Fri Apr  2 04:00:22 PST 1999", "1999-04-02T12:00:22Z"),
    ("Fri Aug 01 10:21:01 2014 -0700", "2014-08-01T17:21:01Z"),
    ("Fri Apr 20 13:38:50 2007", "2007-04-20T13:38:50Z"),
    ("THU JAN 01 22:51:33 1970", "1970-01-01T22:51:33Z"),
    ("5/12/2008", "2008-05-12T12:00:00Z"),
    ("12/31/2008 13:14:15", "2008-12-31T13:14:15Z"),
    ("23/12/2009 16:52:18", "2009-12-23T16:52:18Z"),
    ("13/09/2016", "2016-09-13T12:00:00Z"),
    ("2/22/2016 4:00:00 PM", "2016-02-22T16:00:00Z"),
    ("09/09/2018, 20:47", "2018-09-09T20:47:00Z"),
    ("08/14/18", "2018-08-14T12:00:00Z"),
    ("5/4/05", "2005-05-04T12:00:00Z"),
    ("17/4/13 14:10", "2013-04-17T14:10:00Z"),
    ("2016/01/15 14:27:04", "2016-01-15T14:27:04Z"),
    ("2009/8/26", "2009-08-26T12:00:00Z"),
    ("96/1/02", "1996-01-02T12:00:00Z"),
    ("96/12/2", "1996-12-02T12:00:00Z"),
    ("2017-11-20T13:5836", "2017-11-20T13:58:36Z"),
    ("2018-09-13T18.20.16Z", "2018-09-13T18:20:16Z"),
    ("2006-06-19T12:37:3Z", "2006-06-19T12:37:03Z"),
    ("2012-08-02T19:22:29:93", "2012-08-02T19:22:29Z"),
    ("2008-01-14T19:43:09Z00:00", "2008-01-14T19:43:09Z"),
    ("2016-01-14T11:27:45-6':00", "2016-01-14T17:27:45Z"),
    ("2017-12-31 07:09:59 PM", "2017-12-31T19:09:59Z"),
    ("D:2018-08-02T16:06:22+02'00'", "2018-08-02T14:06:22Z"),
    ("D:20160308221458Z'", "2016-03-08T22:14:58Z"),
    ("D:20210712135012+02'00''", "2021-07-12T11:50:12Z"),
    ("D:20190221101345-6'00'", "2019-02-21T16:13:45Z"),
    ("8/11/2008 14:8:23", "2008-08-11T14:08:23Z"),
    ("Wednesday, 16/02/2011", "2011-02-16T12:00:00Z"),
    (
        "2006-10-18 16:01:00 -0700 (Wed, 18 Oct 2006)",
        "2006-10-18T23:01:00Z",
    ),
    (
        "Mon, 29 Oct 2007 20:20:52 -0500From: someone",
        "2007-10-30T01:20:52Z",
    ),
    (
        "Thu, 27 Aug 1998 08:39:32 +0200 <br>",
        "1998-08-27T06:39:32Z",
    ),
    ("Sat, 19 Jun 93 16:34:27 JST", "1993-06-19T07:34:27Z"),
    ("Wed, 16 Aug 2000 4:19:1 GMT", "2000-08-16T04:19:01Z"),
    ("15 Oct 99 13:47:26 BST", "1999-10-15T13:47:26Z"),
    ("2010-07-28T11:02:12.000CEST", "2010-07-28T09:02:12Z"),
    ("7-JUL-1994 02:36:51.00", "1994-07-07T02:36:51Z"),
    ("Tuesday, 04-Jul-95 06:04:28 GMT", "1995-07-04T06:04:28Z"),
    ("April 18th, 2000", "2000-04-18T12:00:00Z"),
    ("Nov. 21, 1998", "1998-11-21T12:00:00Z"),
];

/// `testRejected`.
const REJECTED: &[&str] = &[
    "0-01-01T00:00:00Z",
    "0-00-00T00:00:00Z",
    "0000-01-01T00:00:00Z",
    "0-00-00",
    "9999-12-31",
    "9999/12/31",
    "10000-01-01T00:59:59+01:00",
    "406-04-16T07:15:00Z",
    "2/23/201",
    "00:15:54",
    "2:16:58",
    "2014-07-18T05:44:470Z",
    "2015:06:12 10:11:",
    "2016-13-01",
    "2016-02-30",
    "23/13/2009",
    "5/12/8",
    "18/02/98 25:00",
    "11658090",
    "137912",
    "1057298",
    "0",
    "+02:00",
    "()",
    "CPY Document Creation Date",
    "Mon, 7 Jan 2008 08:47:01 0100 (CET)",
    "10 Jun 1998 09:07:34 U",
    "December 15, 2005 Report No. LOXA05-004",
    "2015-09-28T04:47:26-16:00",
    "2017-09-12T02:10:54-21:00",
    "2015-09-28T17:25:50+17:00",
    "2017-05-11T19:05:84",
    "D:20080702212348+20'00'",
    "D:2012010510394209'00'",
    "D:191010111144159",
    "D:20170112095532--4'00'",
];

/// `testToMetadataString`.
const STORED: &[(&str, &str)] = &[
    ("2006-07-15T00:01:46+01:00", "2006-07-14T23:01:46Z"),
    ("2016-08-26T07:06Z", "2016-08-26T07:06:00Z"),
    ("2010-01-10T13:00:19", "2010-01-10T13:00:19"),
    ("2013-09-08T04:14:06.006", "2013-09-08T04:14:06"),
    ("2015-08-26T09:39", "2015-08-26T09:39:00"),
    ("D:20041028184621", "2004-10-28T18:46:21"),
    ("D:20080528153425+02'00'", "2008-05-28T13:34:25Z"),
    ("2014:08:21 13:33:53", "2014-08-21T13:33:53"),
    ("Fri, 01 Dec 2000 08:39:07", "2000-12-01T08:39:07"),
    ("Fri, 01 Dec 2006 00:06:00 GMT", "2006-12-01T00:06:00Z"),
    ("2005-11-03", "2005-11-03"),
    ("2015:06:12", "2015-06-12"),
    ("D:20041109", "2004-11-09"),
    ("August 12, 1999", "1999-08-12"),
];

#[test]
fn normalizes_each_family_and_quirk() {
    for &(raw, expected) in NORMALIZED {
        assert_eq!(normalize(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn rejects_what_is_no_date_within_bounds() {
    for &raw in REJECTED {
        assert_eq!(normalize(raw), None, "{raw}");
    }
}

#[test]
fn reads_the_mail_regression_suite() {
    for raw in [
        "Mon, 9 May 16 01:32:00 GMT",
        "9 May 16 01:32:00 GMT",
        "Monday, 9 May 16 01:32:00 GMT",
        "Mon, 9 May 2016 01:32:00 UTC",
        "9 May 2016 01:32:00 UTC",
        "09 May 2016 01:32:00 UTC",
        "Mon, 9 May 2016 01:32:00Z",
        "Mon, 9 May 2016 01:32:00 Z",
        "Mon, 9 May 2016 01:32:00 GMT",
        "Mon, 9 May 2016 01:32:00GMT",
        "Mon, 9 May 2016 01:32:00UTC",
        "Mon, 9 May 2016 3:32:00 GMT+0200",
        "Mon, 9 May 2016 3:32:00 UTC+0200",
        "Mon, 9 May 2016 7:32:00 UTC+0600 (BST)",
        "       9 May 2016 3:32:00 +0200",
        "      Mon, 9 May 2016 3:32:00 +0200",
        "Mon, 9 May 2016 3:32:00 +02:00",
        "9 May 2016 3:32:00 +02:00",
        "Mon, 9 May 2016 3:32:00+02:00",
        "Mon, 9 May 2016 3:32:00+0200",
        "      Sun, 8 May 2016 21:32:00 EDT",
    ] {
        assert_eq!(
            normalize(raw).as_deref(),
            Some("2016-05-09T01:32:00Z"),
            "{raw}"
        );
    }
    for raw in [
        "Mon, 9 May 2016 01:32:00",
        "Monday, 9 May 2016 1:32 AM",
        "May 9 2016 1:32am",
        "May 9 2016 1:32 am",
        "2016-05-09 01:32:00",
    ] {
        assert_eq!(
            normalize(raw).as_deref(),
            Some("2016-05-09T01:32:00Z"),
            "{raw}"
        );
        assert_eq!(
            to_metadata_string(raw).as_deref(),
            Some("2016-05-09T01:32:00"),
            "{raw}"
        );
    }
    for raw in [
        "May 15, 2016",
        "Sun, 15 May 2016",
        "15 May 2016",
        "2016-05-15",
    ] {
        assert_eq!(
            normalize(raw).as_deref(),
            Some("2016-05-15T12:00:00Z"),
            "{raw}"
        );
    }
    assert_eq!(
        normalize("Sun, 8 May 2016 21:32:00 EST").as_deref(),
        Some("2016-05-09T02:32:00Z")
    );
    for raw in [
        "11/14/08",
        "1/14/08",
        "1/2/08",
        "12/1/2008",
        "12/02/1996",
        "96/1/02",
        "96/12/02",
        "96/12/2",
        "1996/12/02",
        "Mon, 29 Jan 96 14:02 GMT",
        "7/20/95 1:12PM",
        "08/14/2000  12:48 AM",
        "8/4/2000  1:48 AM",
        "8/1/03",
        "Wed, 27 Dec 95 11:20:40 EST",
        "26 Aug 00 11:14:52 EDT",
    ] {
        let normalized = normalize(raw).unwrap_or_default();
        assert!(
            normalized.as_str() > "1980" && normalized.as_str() < "2010",
            "{raw} -> {normalized}"
        );
    }
    assert_eq!(normalize("10-10-2022 not a date"), None);
}

#[test]
fn keeps_a_partial_date_where_asked() {
    assert_eq!(
        to_metadata_string_keep_partial("2018").as_deref(),
        Some("2018")
    );
    assert_eq!(
        to_metadata_string_keep_partial("2018-05").as_deref(),
        Some("2018-05")
    );
    assert_eq!(
        to_metadata_string_keep_partial("2018-05-01").as_deref(),
        Some("2018-05-01")
    );
    for junk in [
        "",
        "   ",
        "0",
        "18",
        "0312",
        "2018 2018",
        "[www.example.com]",
    ] {
        assert_eq!(to_metadata_string_keep_partial(junk), None, "{junk}");
    }
}

#[test]
fn bounds_the_year_of_an_instant() {
    assert!(in_year_bounds(1_329_756_262_000));
    assert!(!in_year_bounds(-62_009_370_000_000));
    assert!(!in_year_bounds(920_390_660_309_000));
}

#[test]
fn reads_nothing_of_an_empty_value() {
    assert_eq!(normalize(""), None);
    assert_eq!(normalize("   "), None);
    assert_eq!(parse(""), None);
}

#[test]
fn tells_partial_dates() {
    assert_eq!(parse("2019").unwrap().precision(), Precision::Year);
    assert_eq!(parse("2019-06").unwrap().precision(), Precision::Month);
    assert_eq!(parse("D:2003").unwrap().precision(), Precision::Year);
    assert!(!parse("2019").unwrap().is_full_precision());
    assert_eq!(normalize("2019"), None);
    assert_eq!(normalize("2019-06"), None);
    assert_eq!(normalize("D:2003"), None);
}

#[test]
fn tells_precision_and_zone() {
    let date = parse("2010-01-10T13:00:19").unwrap();
    assert_eq!(date.precision(), Precision::Second);
    assert!(!date.has_zone());
    assert!(parse("2010-01-10T13:00:19Z").unwrap().has_zone());
    assert_eq!(
        parse("2016-08-26T07:06Z").unwrap().precision(),
        Precision::Minute
    );
    assert_eq!(parse("2005-11-03").unwrap().precision(), Precision::Day);
    assert!(parse("2005-11-03").unwrap().is_full_precision());
}

#[test]
fn normalizes_its_own_output_to_itself() {
    for raw in [
        "2006-07-15T00:01:46+01:00",
        "D:20080528153425+02'00'",
        "Fri, 01 Dec 2006 00:06:00 GMT",
        "2005-11-03",
        "2014:08:21 13:33:53",
    ] {
        let normalized = normalize(raw).unwrap();
        assert_eq!(normalize(&normalized), Some(normalized.clone()), "{raw}");
    }
}

#[test]
fn stores_each_family_in_its_form() {
    for &(raw, expected) in STORED {
        assert_eq!(to_metadata_string(raw).as_deref(), Some(expected), "{raw}");
    }
}

#[test]
fn reads_back_what_it_stores() {
    for raw in [
        "2006-07-15T00:01:46+01:00",
        "2010-01-10T13:00:19",
        "2015-08-26T09:39",
        "D:20041028184621",
        "2014:08:21 13:33:53",
        "2005-11-03",
        "Fri, 01 Dec 2006 00:06:00 GMT",
    ] {
        let first = parse(raw).unwrap();
        let stored = first.to_metadata_string();
        let second = parse(&stored).unwrap();
        assert_eq!(first.epoch_millis(), second.epoch_millis(), "{raw}");
        assert_eq!(first.has_zone(), second.has_zone(), "{raw}");
        assert_eq!(
            first.precision() == Precision::Day,
            second.precision() == Precision::Day,
            "{raw}"
        );
        if !first.has_zone() {
            assert_eq!(first.local(), second.local(), "{raw}");
        }
        assert_eq!(to_metadata_string(&stored), Some(stored.clone()), "{raw}");
    }
}

#[test]
fn stores_no_partial_date_or_garbage() {
    assert_eq!(to_metadata_string("2019"), None);
    assert_eq!(to_metadata_string("2019-06"), None);
    assert_eq!(to_metadata_string("0-01-01T00:00:00Z"), None);
}

#[test]
fn reads_a_hostile_value_cheaply() {
    for prefix in ["", "1", "MON ", "JAN "] {
        let hostile = format!("{prefix}{}", "(".repeat(100_000));
        assert_eq!(parse(&hostile), None);
        assert_eq!(parse(&format!("{hostile})X")), None);
    }
    assert!(parse(&format!("2020-01-01T10:00:00Z{}", " ".repeat(1000))).is_some());
}

#[test]
fn keeps_the_zone_of_a_utc_marker_with_a_zero_offset() {
    assert_eq!(
        to_metadata_string("2020-01-01T10:00:00Z00:00").as_deref(),
        Some("2020-01-01T10:00:00Z")
    );
    assert_eq!(
        to_metadata_string("2020-01-01T10:00:00Z0000").as_deref(),
        Some("2020-01-01T10:00:00Z")
    );
    assert!(parse("2020-01-01T10:00:00Z00:00").unwrap().has_zone());
}

#[test]
fn reads_ctime_zone_names() {
    assert_eq!(
        to_metadata_string("Wed Jan 1 10:00:00 IST 2020").as_deref(),
        Some("2020-01-01T10:00:00")
    );
    assert_eq!(
        to_metadata_string("Wed Jan 1 10:00:00 EST 2020").as_deref(),
        Some("2020-01-01T15:00:00Z")
    );
    assert_eq!(
        to_metadata_string("Wed Jan 1 10:00:00 EST 2020 -0500").as_deref(),
        Some("2020-01-01T15:00:00Z")
    );
    assert_eq!(
        to_metadata_string("Wed Jan 1 10:00:00 EST 2020 +0100"),
        None
    );
}

#[test]
fn bounds_offsets_with_their_minutes() {
    assert_eq!(
        normalize("2020-01-01T10:00:00+14:00").as_deref(),
        Some("2019-12-31T20:00:00Z")
    );
    assert_eq!(
        normalize("2020-01-01T10:00:00-12:00").as_deref(),
        Some("2020-01-01T22:00:00Z")
    );
    assert_eq!(
        normalize("2020-01-01T10:00:00+05:45").as_deref(),
        Some("2020-01-01T04:15:00Z")
    );
    assert_eq!(normalize("2020-01-01T10:00:00+14:45"), None);
    assert_eq!(normalize("2020-01-01T10:00:00-12:30"), None);
}

#[test]
fn leaves_a_date_alone_without_a_zone() {
    assert_eq!(
        to_metadata_string("2020-01-01+14:00").as_deref(),
        Some("2020-01-01")
    );
    assert_eq!(
        normalize("2020-01-01+14:00").as_deref(),
        Some("2020-01-01T12:00:00Z")
    );
    assert!(!parse("2020-01-01-12:00").unwrap().has_zone());
}
