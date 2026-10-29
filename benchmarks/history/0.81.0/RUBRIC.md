# Page-usefulness judging

Input: a JSONL file, one page per line with fields topic, url, title, text (start of the page's extracted content).
For EACH line, judge how useful the page is for someone searching the topic's query on that university's site,
based mainly on the page content (url/title as supporting evidence).

Topics and queries:
- intl: "international students admission tuition scholarship visa" — a prospective international student: how to apply, fees, scholarships, visas, language requirements, international student support.
- grad: "graduate admission requirements PhD application deadline" — applying to a master's/PhD program: entry requirements, how/when to apply, deadlines, graduate admissions.
- housing: "student housing accommodation residence" — university housing/accommodation/residence halls, applying for housing, costs.
- cs: "computer science department faculty research" — the computer science department, its faculty/people, its research areas/groups.
- library: "library opening hours borrowing" — library opening hours, borrowing/loan rules, library access and services.

Labels:
- U (useful): directly about the topic and answers part of the query (a page on visas for international applicants; a residence hall page; the CS department's faculty list; library opening hours).
- M (marginal): related but not directly what was asked (a general admissions page for intl; one professor's personal page for cs; a student-services hub linking to housing; a library database guide).
- X (not useful): unrelated to the topic, login/error/empty page, news/events not about the topic, other departments' programmes, a generic homepage with nothing on the topic.

Judge each page independently and honestly, neither lenient nor strict. The labels must come from reading each page,
not from keyword rules.

Output: one JSON object per input line, same order and count: {"topic": ..., "url": ..., "label": "U"|"M"|"X"}
