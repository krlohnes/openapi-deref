use std::path::PathBuf;
use std::str::FromStr;

use indexmap::IndexMap;
use jsonpath_rust::JsonPathQuery;
use openapiv3::v3_1::{
    Callback, Components, Example, Header, Link, OpenApi as OpenApiV3_1, Parameter, ParameterData,
    PathItem, ReferenceOr, RequestBody, Response, SecurityScheme,
};
use openapiv3::versioned::OpenApi;
use snafu::prelude::*;

pub struct OpenApiDereferenceer {
    json: serde_json::Value,
    openapi: OpenApiV3_1,
}

#[derive(Debug, Snafu)]
pub enum OpenApiError {
    #[snafu(display("Error parsing open api spec"))]
    ParsingError,
    #[snafu(display("References must be in the same file with the format #..."))]
    UnsupportedRefFormat,
    #[snafu(display("Unsupported open api version"))]
    UnsupportedOpenApiVersion,
}

impl FromStr for OpenApiDereferenceer {
    type Err = OpenApiError;
    fn from_str(the_str: &str) -> Result<Self, OpenApiError> {
        let json: serde_json::Value =
            serde_json::from_str(the_str).map_err(|_| OpenApiError::ParsingError)?;
        let openapi: OpenApi =
            serde_json::from_value(json.clone()).map_err(|_| OpenApiError::ParsingError)?;
        match openapi {
            OpenApi::Version31(openapi) => Ok(OpenApiDereferenceer { json, openapi }),
            _ => return Err(OpenApiError::UnsupportedOpenApiVersion),
        }
    }
}

pub fn ref_to_json_path(ref_str: &str) -> Result<String, OpenApiError> {
    let mut chars = ref_str.chars();
    let first_char = chars.next();
    if first_char.is_none() || first_char.unwrap() != '#' {
        return Err(OpenApiError::UnsupportedRefFormat);
    }
    let path_str: String = chars.collect();
    let path = PathBuf::from(&path_str);
    let mut json_path: String = "".into();
    for p in path.into_iter() {
        if let Some(p) = p.to_str() {
            json_path += ".";
            json_path += p;
        }
    }
    Ok(json_path)
}

impl OpenApiDereferenceer {
    pub fn dereference(mut self) -> Result<OpenApiV3_1, OpenApiError> {
        let components: Option<Components> = self.openapi.components.take();
        self.openapi.components = self.dereference_components(components)?;
        Ok(self.openapi)
    }

    fn dereference_header(&self, mut header: Header) -> Result<Header, OpenApiError> {
        let res: Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError> = header
            .examples
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        header.examples = res?;
        Ok(header)
    }

    fn dereference_parameter_data(
        &self,
        mut parameter_data: ParameterData,
    ) -> Result<ParameterData, OpenApiError> {
        //Note examples can have external values, but we don't care at the moment.
        let res: Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError> = parameter_data
            .examples
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        parameter_data.examples = res?;
        Ok(parameter_data)
    }

    fn dereference_parameter(&self, parameter: Parameter) -> Result<Parameter, OpenApiError> {
        match parameter {
            Parameter::Query {
                parameter_data,
                allow_reserved,
                style,
                allow_empty_value,
            } => Ok(Parameter::Query {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                allow_reserved,
                style,
                allow_empty_value,
            }),
            Parameter::Header {
                parameter_data,
                style,
            } => Ok(Parameter::Header {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
            Parameter::Path {
                parameter_data,
                style,
            } => Ok(Parameter::Path {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
            Parameter::Cookie {
                parameter_data,
                style,
            } => Ok(Parameter::Cookie {
                parameter_data: self.dereference_parameter_data(parameter_data)?,
                style,
            }),
        }
    }

    fn dereference_response(&self, mut response: Response) -> Result<Response, OpenApiError> {
        let res: Result<IndexMap<String, ReferenceOr<Header>>, OpenApiError> = response
            .headers
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        response.headers = res?;
        let res: Result<IndexMap<String, ReferenceOr<Link>>, OpenApiError> = response
            .links
            .into_iter()
            .map(|(k, v)| {
                let new_v = self.dereference_reference(v)?;
                Ok((k, new_v))
            })
            .collect();
        response.links = res?;
        Ok(response)
    }

    fn dereference_components(
        &self,
        components: Option<Components>,
    ) -> Result<Option<Components>, OpenApiError> {
        //Extensions can't be references
        //Schemas can't be references
        if let Some(mut components) = components {
            let res: Result<IndexMap<String, ReferenceOr<SecurityScheme>>, OpenApiError> =
                components
                    .security_schemes
                    .into_iter()
                    .map(|(k, v)| {
                        let new_v = self.dereference_reference(v)?;
                        Ok((k, new_v))
                    })
                    .collect();
            components.security_schemes = res?;
            let res: Result<IndexMap<String, ReferenceOr<Response>>, OpenApiError> = components
                .responses
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    if let ReferenceOr::DereferencedReference {
                        reference,
                        summary,
                        description,
                        item,
                    } = new_v
                    {
                        return Ok((
                            k,
                            ReferenceOr::DereferencedReference {
                                reference,
                                summary,
                                description,
                                item: self.dereference_response(item)?,
                            },
                        ));
                    }
                    Ok((k, new_v))
                })
                .collect();
            components.responses = res?;
            let res: Result<IndexMap<String, ReferenceOr<Parameter>>, OpenApiError> = components
                .parameters
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    if let ReferenceOr::DereferencedReference {
                        reference,
                        summary,
                        description,
                        item,
                    } = new_v
                    {
                        return Ok((
                            k,
                            ReferenceOr::DereferencedReference {
                                reference,
                                summary,
                                description,
                                item: self.dereference_parameter(item)?,
                            },
                        ));
                    }
                    Ok((k, new_v))
                })
                .collect();
            components.parameters = res?;
            let res: Result<IndexMap<String, ReferenceOr<Example>>, OpenApiError> = components
                .examples
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.examples = res?;
            let res: Result<IndexMap<String, ReferenceOr<RequestBody>>, OpenApiError> = components
                .request_bodies
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.request_bodies = res?;
            let res: Result<IndexMap<String, ReferenceOr<Header>>, OpenApiError> = components
                .headers
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.headers = res?;
            let res: Result<IndexMap<String, ReferenceOr<Header>>, OpenApiError> = components
                .headers
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    if let ReferenceOr::DereferencedReference {
                        reference,
                        summary,
                        description,
                        item,
                    } = new_v
                    {
                        return Ok((
                            k,
                            ReferenceOr::DereferencedReference {
                                reference,
                                summary,
                                description,
                                item: self.dereference_header(item)?,
                            },
                        ));
                    }
                    Ok((k, new_v))
                })
                .collect();
            components.headers = res?;

            let res: Result<IndexMap<String, ReferenceOr<Link>>, OpenApiError> = components
                .links
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.links = res?;

            //I don't think we care about callbacks for the moment.
            let res: Result<IndexMap<String, ReferenceOr<Callback>>, OpenApiError> = components
                .callbacks
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.callbacks = res?;

            //TODO handle the path item here. This is a big chunk of refs
            let res: Result<IndexMap<String, ReferenceOr<PathItem>>, OpenApiError> = components
                .path_items
                .into_iter()
                .map(|(k, v)| {
                    let new_v = self.dereference_reference(v)?;
                    Ok((k, new_v))
                })
                .collect();
            components.path_items = res?;
            Ok(Some(components))
        } else {
            Ok(None)
        }
    }

    fn dereference_reference<T: serde::de::DeserializeOwned>(
        &self,
        v: ReferenceOr<T>,
    ) -> Result<ReferenceOr<T>, OpenApiError> {
        match v {
            ReferenceOr::Item(i) => Ok(ReferenceOr::Item(i)),
            ReferenceOr::Reference {
                reference,
                summary,
                description,
            } => {
                let jp = ref_to_json_path(&reference)?;
                let item: T = serde_json::from_value(
                    (&self.json)
                        .clone()
                        .path(&jp)
                        .map_err(|_| OpenApiError::ParsingError)?,
                )
                .unwrap();
                Ok(ReferenceOr::DereferencedReference {
                    reference,
                    summary,
                    description,
                    item,
                })
            }
            ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item,
            } => Ok(ReferenceOr::DereferencedReference {
                reference,
                summary,
                description,
                item,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    fn is_reference<T>(reference: (&String, &ReferenceOr<T>)) -> bool {
        if let ReferenceOr::Reference {
            reference: _,
            summary: _,
            description: _,
        } = reference.1
        {
            return true;
        }
        false
    }
    #[test]
    pub fn test_github_api_from_3_1_api() -> Result<()> {
        //NOTE: This is a sanity check. the github api doesn't have _everything_, but it
        //seems like if that passes, we're reasonably good. We might want something more
        //comprehensive in the future
        let spec = std::fs::read_to_string("oai_examples/api.github.com.json")?;
        let dereferenceer = OpenApiDereferenceer::from_str(&spec)?;
        let dereferenceed = dereferenceer.dereference()?;

        assert!(dereferenceed.components.is_some());
        let components = dereferenceed.components.unwrap();
        assert!(!components.security_schemes.iter().any(is_reference));
        assert!(!components.responses.iter().any(is_reference));
        assert!(!components.parameters.iter().any(is_reference));
        assert!(!components.examples.iter().any(is_reference));
        assert!(!components.request_bodies.iter().any(is_reference));
        assert!(!components.headers.iter().any(is_reference));
        assert!(!components.links.iter().any(is_reference));
        assert!(!components.callbacks.iter().any(is_reference));
        assert!(!components.path_items.iter().any(is_reference));
        Ok(())
    }

    #[test]
    pub fn test_3_0_api_is_err() -> Result<()> {
        let spec = std::fs::read_to_string("oai_examples/petstore-expanded.json")?;
        assert!(OpenApiDereferenceer::from_str(&spec).is_err());
        Ok(())
    }
}
