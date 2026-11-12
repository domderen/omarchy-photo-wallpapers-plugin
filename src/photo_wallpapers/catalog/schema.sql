PRAGMA foreign_keys=ON;
CREATE TABLE catalog_metadata (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE photos (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE people (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE albums (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE photo_people (asset_id TEXT REFERENCES photos(id), person_id TEXT REFERENCES people(id), PRIMARY KEY(asset_id,person_id));
CREATE TABLE photo_albums (asset_id TEXT REFERENCES photos(id), album_id TEXT REFERENCES albums(id), PRIMARY KEY(asset_id,album_id));
CREATE TABLE faces (asset_id TEXT REFERENCES photos(id), person_id TEXT REFERENCES people(id), data TEXT NOT NULL);
