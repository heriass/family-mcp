//! School dates through the binary: packages/inna-mcp/test/dates.test.ts:4's table, each source
//! sent as an upstream date and read back from the tool's `dates` record. Text dates come in the
//! overview's announcements, numeric ones as course-grade assignment dates (the shapes that carry
//! them). NaN and Infinity are not in the table here: JSON cannot carry them, so no upstream
//! answer holds them. Needs no Bun; needs the `test-origin` feature.
#![cfg(feature = "test-origin")]

mod support;

use serde_json::{Value, json};
use support::Reply;
use support::case::Case;

/// dates.test.ts's text sources and what `normalizeDate` makes of them.
const TEXT: [(&str, Option<&str>); 30] = [
    ("2040-02-29", Some("2040-02-29")),
    ("29.2.2040", Some("2040-02-29")),
    (" 02.01.2040 ", Some("2040-01-02")),
    ("02.01.2040 9:05", Some("2040-01-02T09:05:00.000Z")),
    ("2040-01-02T10:15:30", Some("2040-01-02T10:15:30.000Z")),
    ("2040-01-02 00:00", Some("2040-01-02T00:00:00.000Z")),
    ("2040-01-02T23:59:59.999Z", Some("2040-01-02T23:59:59.999Z")),
    (
        "2040-01-02T00:15:00+01:00",
        Some("2040-01-01T23:15:00.000Z"),
    ),
    (
        "2040-01-02T23:15:00-02:00",
        Some("2040-01-03T01:15:00.000Z"),
    ),
    ("2040-01-02T01:02:03.1Z", Some("2040-01-02T01:02:03.100Z")),
    ("", None),
    ("   ", None),
    ("2041-02-29", None),
    ("1900-02-29", None),
    ("31.04.2040", None),
    ("00.01.2040", None),
    ("2040-00-02", None),
    ("2040-13-02", None),
    ("0000-01-01", None),
    ("01/02/2040", None),
    ("2040-01-02 trailing data", None),
    ("02.01.2040 malformed time", None),
    ("2040-01-02T24:00:00", None),
    ("2040-01-02T12:60:00", None),
    ("2040-01-02T12:00:60", None),
    ("2040-01-02T12:00:00+24:00", None),
    ("2040-01-02T12:00:00+01:60", None),
    ("2040-01-02T12:00:00.1234Z", None),
    ("2040-01-02T12:00:00 UTC", None),
    ("2209032000000", None),
];

/// `parseDates`'s status: blank text and 0 are missing, anything else unparsed is unrecognized.
fn expected(source: &Value, iso: Option<&str>) -> Value {
    let missing = source.as_str().is_some_and(|text| text.trim().is_empty()) || *source == 0;
    let status = match (iso, missing) {
        (Some(_), _) => "parsed",
        (None, true) => "missing",
        (None, false) => "unrecognized",
    };
    json!({"iso": iso, "status": status})
}

/// dates.test.ts:4
#[test]
fn school_dates_normalize_strictly_to_utc_without_changing_date_only_values() {
    // `Date.parse('2040-01-02T12:00:00Z')`, then 1, 0, 1.5 and Number.MAX_SAFE_INTEGER.
    let numbers: [(Value, Option<&str>); 5] = [
        (
            json!(2_209_118_400_000_i64),
            Some("2040-01-02T12:00:00.000Z"),
        ),
        (json!(1), Some("1970-01-01T00:00:00.001Z")),
        (json!(0), None),
        (json!(1.5), None),
        (json!(9_007_199_254_740_991_i64), None),
    ];
    let case = Case::new("dates", false);
    let provider = case.provider.clone();
    let assignments = numbers.clone();
    let mut client = case.client_with(move |request| {
        let reply = provider.answer(request);

        match request.line().as_str() {
            "GET /api/Announcements/GetStudentAnnouncements" => {
                let announcements: Vec<Value> = TEXT
                    .iter()
                    .enumerate()
                    .map(|(index, (date, _))| {
                        json!({
                            "announcementId": (index + 1).to_string(),
                            "date": date,
                            "title": "Synthetic announcement",
                            "sender": "Synthetic school",
                            "contentHtml": "<p>Synthetic notice</p>",
                            "hasOpened": false,
                        })
                    })
                    .collect();
                Reply::json(200, &Value::Array(announcements))
            }
            "GET /api/GetAssignments/Groups/7/StudentProjects" => {
                let list: Vec<Value> = assignments
                    .iter()
                    .enumerate()
                    .map(|(index, (date, _))| {
                        json!({
                            "id": index + 1,
                            "name": "Synthetic assessment",
                            "type": 0,
                            "weight": 20,
                            "grade": "8",
                            "returnDate": 1,
                            "assignDate": date,
                            "handedIn": true,
                        })
                    })
                    .collect();
                Reply::json(200, &json!({"assignments": list, "categories": []}))
            }
            _ => reply,
        }
    });

    let overview = client.call("inna_get_overview", json!({})).unwrap();
    let announcements = overview["announcements"].as_array().unwrap();
    assert_eq!(announcements.len(), TEXT.len(), "{overview}");

    for (index, (source, iso)) in TEXT.iter().enumerate() {
        let announcement = &announcements[index];
        assert_eq!(announcement["announcementId"], (index + 1).to_string());
        assert_eq!(announcement["date"], *source);
        assert_eq!(
            announcement["dates"],
            json!({"date": expected(&json!(source), *iso)}),
            "{source:?}"
        );
    }

    let grades = client
        .call("inna_get_course_grades", json!({"groupId": "7"}))
        .unwrap();
    let list = grades["assignments"].as_array().unwrap();
    assert_eq!(list.len(), numbers.len(), "{grades}");

    for (index, (source, iso)) in numbers.iter().enumerate() {
        assert_eq!(
            list[index]["dates"]["assignDate"],
            expected(source, *iso),
            "{source}"
        );
    }
}
