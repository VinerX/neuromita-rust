use std::{path::Path, time::Duration};

use rusqlite::{OpenFlags, params_from_iter, types::ValueRef};

use crate::timeline::domain::TimelineError;

use super::storage;

pub(super) struct Connection(rusqlite::Connection);

impl Connection {
    pub(super) fn open(path: &Path) -> Result<Self, TimelineError> {
        let connection = rusqlite::Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
        )
        .map_err(|error| storage(error.to_string()))?;
        connection
            .busy_timeout(Duration::from_secs(10))
            .map_err(|error| storage(error.to_string()))?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|error| storage(error.to_string()))?;
        Ok(Self(connection))
    }

    pub(super) fn exec(&self, sql: &str) -> Result<(), TimelineError> {
        self.0
            .execute_batch(sql)
            .map_err(|error| storage(error.to_string()))
    }

    pub(super) fn query(
        &self,
        sql: &str,
        params: &[&str],
    ) -> Result<Vec<Vec<String>>, TimelineError> {
        let mut statement = self
            .0
            .prepare(sql)
            .map_err(|error| storage(error.to_string()))?;
        let count = statement.column_count();
        let mut rows = statement
            .query(params_from_iter(params.iter()))
            .map_err(|error| storage(error.to_string()))?;
        let mut results = Vec::new();
        while let Some(row) = rows.next().map_err(|error| storage(error.to_string()))? {
            let mut fields = Vec::with_capacity(count);
            for index in 0..count {
                let value = match row
                    .get_ref(index)
                    .map_err(|error| storage(error.to_string()))?
                {
                    ValueRef::Null => String::new(),
                    ValueRef::Integer(value) => value.to_string(),
                    ValueRef::Real(value) => value.to_string(),
                    ValueRef::Text(value) => std::str::from_utf8(value)
                        .map_err(|error| storage(error.to_string()))?
                        .to_owned(),
                    ValueRef::Blob(_) => return Err(storage("unexpected SQLite binary value")),
                };
                fields.push(value);
            }
            results.push(fields);
        }
        Ok(results)
    }

    pub(super) fn write(&self, sql: &str, params: &[&str]) -> Result<(), TimelineError> {
        self.0
            .execute(sql, params_from_iter(params.iter()))
            .map_err(|error| storage(error.to_string()))?;
        Ok(())
    }
}
