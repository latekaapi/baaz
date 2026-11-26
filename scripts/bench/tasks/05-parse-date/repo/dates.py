MONTHS = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"]


def parse_date(text):
    """'26 Nov 2026' -> (2026, 11, 26)"""
    day, mon, year = text.split()
    return int(year), MONTHS.index(mon.lower()), int(day)
