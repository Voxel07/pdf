import type { Field } from './api';

// DocuSeal adds numeric suffixes to repeated placements of the same field.
function label(field: Field) {
  return field.name.toLowerCase().replace(/\s+\d+$/, '').replace(/[^a-zäöüß]/g, '');
}

const givenNames = new Set(['vorname', 'vornamen', 'firstname', 'forename', 'givenname', 'givennames']);
const surnames = new Set(['nachname', 'familienname', 'lastname', 'surname', 'familyname']);
const fullNames = new Set(['fullname', 'vollername', 'vollständigername', 'vorundnachname', 'vornamenachname', 'vornameundnachname', 'firstandlastname', 'firstnamelastname', 'nameandsurname', 'namesurname']);
const postcodes = new Set(['zip', 'zipcode', 'postalcode', 'postcode', 'plz', 'postleitzahl']);

export function signerPrefill(fields: Field[], fullName: string, zip: string): Record<string, string> {
  const name = fullName.trim().replace(/\s+/g, ' ');
  const [family, given] = name.split(',').map((part) => part.trim());
  const [first, ...rest] = name.split(' ');
  const firstName = given || first;
  const lastName = given ? family : rest.join(' ');
  const hasGivenName = new Set(fields.filter((f) => f.type === 'text' && givenNames.has(label(f))).map((f) => f.section));
  const hasSurname = new Set(fields.filter((f) => f.type === 'text' && surnames.has(label(f))).map((f) => f.section));
  const defaults: Record<string, string> = {};
  for (const field of fields) {
    if (field.autofill) continue;
    const key = label(field);
    if (postcodes.has(key) && (field.type === 'text' || field.type === 'number')) {
      if (zip && (field.type === 'text' || /^\d+$/.test(zip))) defaults[field.id] = zip;
    } else if (field.type === 'text') {
      if (givenNames.has(key)) defaults[field.id] = firstName;
      else if (surnames.has(key)) defaults[field.id] = lastName;
      else if (key === 'name') {
        if (hasGivenName.has(field.section)) defaults[field.id] = lastName;
        else if (hasSurname.has(field.section)) defaults[field.id] = firstName;
        else defaults[field.id] = name;
      }
      else if (fullNames.has(key)) defaults[field.id] = name;
      else if (key === 'namevorname' || key === 'nachnamevorname') defaults[field.id] = [lastName, firstName].filter(Boolean).join(', ');
    }
  }
  return defaults;
}
